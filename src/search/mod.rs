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
    types::ScopeFilter,
};

/// BM25 keyword index (Tantivy, in-RAM).
pub mod bm25;
/// Reciprocal rank fusion for merging result lists.
pub mod fusion;

pub use bm25::{LexicalDoc, LexicalIndex, LexicalOp};
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
pub async fn rebuild_lexical_from_repo(
    repo: &Arc<MemoryRepo>,
    lexical: &Arc<LexicalIndex>,
) -> Result<usize, MemoryError> {
    let token = lexical.begin_rebuild();
    let memories = repo.list_memories(None).await?;
    let docs: Vec<LexicalDoc> = memories
        .into_iter()
        .map(|m| LexicalDoc {
            qualified_name: m.mem_ref().qualified_path(),
            name: m.name.to_string(),
            content: m.content,
        })
        .collect();
    let lexical = Arc::clone(lexical);
    traced_spawn_blocking(move || lexical.rebuild_from(token, docs))
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
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
            Ok(count) => info!(count, "lexical index repaired from git truth"),
            Err(e) => warn!(error = %e, "lexical repair failed — index stays degraded"),
        }
    });
}
