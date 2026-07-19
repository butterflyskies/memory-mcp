//! Hybrid retrieval — semantic (embedding) and lexical (BM25) strategies
//! run in parallel and merged via reciprocal rank fusion.
//!
//! Whole-memory embeddings dilute short exact phrases inside long
//! multi-topic memories: a one-line fact in a 3 KB memory contributes almost
//! nothing to the document vector, so literal-phrase queries rank it below
//! short memories that are topically adjacent but wrong. The lexical
//! strategy catches exactly those queries; fusion lets either strategy
//! surface a hit the other missed.

use std::sync::Arc;

use tracing::{info, warn, Instrument};

use crate::{
    embedding::EmbeddingBackend,
    error::MemoryError,
    index::VectorStore,
    repo::{traced_spawn_blocking, MemoryRepo},
    repo_router::RepoRouter,
    types::ScopeFilter,
};

/// BM25 keyword index (Tantivy, in-RAM).
pub mod bm25;
/// Reciprocal rank fusion for merging result lists.
pub mod fusion;

pub use bm25::{LexicalDoc, LexicalIndex, LexicalOp, LexicalStatus};
pub use fusion::{reciprocal_rank_fusion, FusedHit};

/// Run semantic and lexical retrieval in parallel and merge the ranked
/// lists with reciprocal rank fusion.
///
/// Each strategy contributes up to `limit` candidates; the fused list is
/// truncated back to `limit`. A semantic failure (embedding or vector index)
/// is fatal — it preserves recall's pre-hybrid error surface. A lexical
/// failure only degrades: the error is logged and semantic results are
/// returned alone, so the new subsystem can never break recall.
pub async fn hybrid_search(
    embedding: &dyn EmbeddingBackend,
    index: &dyn VectorStore,
    lexical: &Arc<LexicalIndex>,
    filter: &ScopeFilter,
    query: &str,
    limit: usize,
) -> Result<Vec<FusedHit>, MemoryError> {
    let span = tracing::info_span!(
        "search.hybrid",
        ?filter,
        limit,
        semantic_candidates = tracing::field::Empty,
        lexical_candidates = tracing::field::Empty,
    );
    async move {
        let semantic_fut = async {
            let start = std::time::Instant::now();
            let query_vector = embedding.embed_one(query).await?;
            info!(embed_ms = start.elapsed().as_millis(), "query embedded");

            let start = std::time::Instant::now();
            let results = index.search(filter, &query_vector, limit)?;
            info!(
                search_ms = start.elapsed().as_millis(),
                candidates = results.len(),
                "semantic index searched"
            );
            Ok::<_, MemoryError>(results)
        };

        let lexical_index = Arc::clone(lexical);
        let lexical_filter = filter.clone();
        let lexical_query = query.to_string();
        let lexical_fut = traced_spawn_blocking(move || {
            lexical_index.search(&lexical_filter, &lexical_query, limit)
        });

        let (semantic_result, lexical_result) = tokio::join!(semantic_fut, lexical_fut);

        let semantic: Vec<(String, f32)> = semantic_result?
            .into_iter()
            .map(|(_key, qualified_name, distance)| (qualified_name, distance))
            .collect();

        let lexical: Vec<(String, f32)> = match lexical_result {
            Ok(Ok(hits)) => hits,
            Ok(Err(e)) => {
                warn!(error = %e, "lexical search failed — returning semantic results only");
                Vec::new()
            }
            Err(e) => {
                warn!(error = %e, "lexical search task failed — returning semantic results only");
                Vec::new()
            }
        };

        tracing::Span::current().record("semantic_candidates", semantic.len());
        tracing::Span::current().record("lexical_candidates", lexical.len());

        Ok(reciprocal_rank_fusion(&semantic, &lexical, limit))
    }
    .instrument(span)
    .await
}

/// Rebuild the lexical index from every memory in the repository.
///
/// The lexical index lives in RAM only, so this runs on every startup —
/// unlike the vector index, which is persisted because embedding is
/// expensive. The rebuild is a single Tantivy commit and runs on the
/// blocking pool. Returns the number of indexed memories.
///
/// This is also the repair path for a degraded index (ADR-0039): the
/// rebuild token is captured *before* the repo listing, so divergence
/// events or mirrors racing the listing keep the index flagged and a
/// follow-up rebuild converges instead of silently losing them.
///
/// Every failure while obtaining or applying repository truth — including
/// the repo listing itself, *before* the `rebuild_from` seam — marks the
/// index rebuild-required. Without that, a listing failure on a fresh
/// index would leave the epochs at 0/0: healthy-but-empty, with recall
/// never scheduling repair (the #314 pre-list gap).
pub async fn rebuild_lexical_from_repo(
    repo: &Arc<MemoryRepo>,
    lexical: &Arc<LexicalIndex>,
) -> Result<usize, MemoryError> {
    let token = lexical.begin_rebuild();
    let memories = match repo.list_memories(None).await {
        Ok(memories) => memories,
        Err(e) => {
            lexical.mark_rebuild_required("repository listing for lexical rebuild failed");
            return Err(e);
        }
    };
    let docs: Vec<LexicalDoc> = memories
        .into_iter()
        .map(|m| LexicalDoc {
            qualified_name: m.mem_ref().qualified_path(),
            name: m.name.to_string(),
            content: m.content,
        })
        .collect();
    let index = Arc::clone(lexical);
    match traced_spawn_blocking(move || index.rebuild_from(token, docs)).await {
        Ok(result) => result,
        Err(e) => {
            // The blocking rebuild task died (panic/shutdown); whether the
            // Tantivy commit landed is unknowable, so stay rebuild-required.
            lexical.mark_rebuild_required("lexical rebuild task did not run to completion");
            Err(MemoryError::Join(e.to_string()))
        }
    }
}

/// Rebuild the lexical index from every repository owned by a router.
///
/// Unlike the user-facing aggregate list operation, this is strict: failure
/// to read any mapped repository leaves the index degraded instead of
/// treating a partial aggregate as authoritative git truth.
pub async fn rebuild_lexical_from_router(
    router: &RepoRouter,
    lexical: &Arc<LexicalIndex>,
) -> Result<usize, MemoryError> {
    let token = lexical.begin_rebuild();
    let memories = match router.list_memories_strict().await {
        Ok(memories) => memories,
        Err(e) => {
            lexical.mark_rebuild_required("repository listing for lexical rebuild failed");
            return Err(e);
        }
    };
    let docs: Vec<LexicalDoc> = memories
        .into_iter()
        .map(|m| LexicalDoc {
            qualified_name: m.mem_ref().qualified_path(),
            name: m.name.to_string(),
            content: m.content,
        })
        .collect();
    let index = Arc::clone(lexical);
    match traced_spawn_blocking(move || index.rebuild_from(token, docs)).await {
        Ok(result) => result,
        Err(e) => {
            lexical.mark_rebuild_required("lexical rebuild task did not run to completion");
            Err(MemoryError::Join(e.to_string()))
        }
    }
}

/// Spawn a background repair rebuild if the lexical index is degraded and
/// no repair is already running.
///
/// Deterministic repair per ADR-0039: rebuild from git truth on the
/// blocking pool, single-flight. Recall serves semantic-only for the whole
/// degraded window (search errors until the rebuild converges). Failures
/// leave the index degraded; the next trigger retries.
pub fn spawn_lexical_repair(repo: &Arc<MemoryRepo>, lexical: &Arc<LexicalIndex>) {
    if !lexical.is_degraded() || !lexical.try_claim_repair() {
        return;
    }
    let repo = Arc::clone(repo);
    let lexical = Arc::clone(lexical);
    tokio::spawn(async move {
        let result = rebuild_lexical_from_repo(&repo, &lexical).await;
        lexical.finish_repair();
        match result {
            // `rebuild_from` deliberately returns Ok while re-flagging the
            // index when a mirror raced its repo listing (or a divergence
            // event landed mid-rebuild) — a raced outcome is not a repair
            // receipt, so the log must not claim convergence.
            Ok(count) if lexical.is_degraded() => info!(
                count,
                "lexical rebuild completed but the index was re-flagged during \
                 the rebuild (raced mirror or new divergence) — still degraded, \
                 the next trigger repairs again"
            ),
            Ok(count) => info!(count, "lexical index repaired from git truth"),
            Err(e) => warn!(error = %e, "lexical repair failed — index stays degraded"),
        }
    });
}

/// Spawn a single-flight repair from all repositories owned by a router.
pub fn spawn_lexical_repair_for_router(router: &RepoRouter, lexical: &Arc<LexicalIndex>) {
    if !lexical.is_degraded() || !lexical.try_claim_repair() {
        return;
    }
    let router = router.clone();
    let lexical = Arc::clone(lexical);
    tokio::spawn(async move {
        let result = rebuild_lexical_from_router(&router, &lexical).await;
        lexical.finish_repair();
        match result {
            Ok(count) if lexical.is_degraded() => info!(
                count,
                "lexical rebuild completed but the index was re-flagged during \
                 the rebuild (raced mirror or new divergence) — still degraded, \
                 the next trigger repairs again"
            ),
            Ok(count) => info!(count, "lexical index repaired from all git repositories"),
            Err(e) => warn!(error = %e, "lexical repair failed — index stays degraded"),
        }
    });
}
