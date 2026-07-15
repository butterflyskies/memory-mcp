use std::{sync::Arc, time::Instant};

/// Maximum number of characters included in recall result snippets.
/// Content longer than this is truncated and flagged with `truncated: true`.
const SNIPPET_MAX_CHARS: usize = 500;

use chrono::Utc;
use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        tool::{Extension, ToolCallContext},
        wrapper::Parameters,
    },
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, Meta, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use tracing::{info, warn, Instrument};

/// Extract the `Mcp-Session-Id` header from HTTP request parts.
///
/// Returns `"unknown"` if the header is absent or not valid UTF-8.
/// Truncates to 128 chars to bound span field size from untrusted input.
fn extract_session_id(parts: &http::request::Parts) -> String {
    let raw = parts
        .headers
        .get("mcp-session-id")
        .and_then(|v: &http::HeaderValue| v.to_str().ok())
        .unwrap_or("unknown");
    if raw.len() > 128 {
        let truncated: String = raw.chars().take(128).collect();
        format!("{truncated}…")
    } else {
        raw.to_owned()
    }
}

use crate::{
    embedding::EmbeddingBackend,
    error::MemoryError,
    index::VectorStore,
    recall_log::{BatchVerdict, RecallLog, RecallResult},
    repo::{traced_spawn_blocking, MemoryRepo},
    search::{hybrid_search, spawn_lexical_repair, FusedHit, LexicalDoc, LexicalIndex, LexicalOp},
    types::{
        parse_qualified_name, AppState, BatchMarkAppliedArgs, ChangedMemories, EditArgs,
        ForgetArgs, ListArgs, MarkAppliedArgs, Memory, MemoryMetadata, MemoryName, MemoryRef,
        MoveArgs, PullResult, ReadArgs, RecallArgs, RecallStatsArgs, ReindexStats, RememberArgs,
        Scope, ScopeFilter, SyncArgs,
    },
};

/// Owned counterpart of [`BatchVerdict`] for moving into `spawn_blocking`.
struct OwnedBatchVerdict {
    recall_id: String,
    memory_name: String,
    verdict: String,
    application_note: Option<String>,
    confidence: String,
}

/// MCP server implementation.
///
/// Each tool method is an async handler that calls into the backing subsystems
/// (git repo, embedding engine, HNSW index) and returns structured JSON.
#[derive(Clone)]
pub struct MemoryServer {
    state: Arc<AppState>,
    // Read by the #[tool_router] macro-generated ServerHandler impl;
    // rustc's dead-code analysis can't see through proc-macro output.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// Maximum allowed content size in bytes (1 MiB).
const MAX_CONTENT_SIZE: usize = 1_048_576;

const SERVER_PROCESSING_DURATION_META_KEY: &str = "memory-mcp/serverProcessingDurationMs";

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn finish_tool_call(
    tool_name: &str,
    start: Instant,
    result: Result<CallToolResult, ErrorData>,
) -> Result<CallToolResult, ErrorData> {
    let server_processing_duration_ms = elapsed_ms(start);
    match result {
        Ok(mut result) => {
            let mut meta = result.meta.take().unwrap_or_else(Meta::new);
            meta.insert(
                SERVER_PROCESSING_DURATION_META_KEY.to_string(),
                server_processing_duration_ms.into(),
            );
            result.meta = Some(meta);
            info!(
                tool_name,
                server_processing_duration_ms,
                outcome = "success",
                "tool call completed"
            );
            Ok(result)
        }
        Err(error) => {
            info!(
                tool_name,
                server_processing_duration_ms,
                outcome = "error",
                "tool call completed"
            );
            Err(error)
        }
    }
}

struct EditStageTiming {
    start: Instant,
    stage: &'static str,
    outcome: &'static str,
    read_ms: u64,
    embed_total_ms: u64,
    index_ms: u64,
    repo_save_ms: u64,
}

impl EditStageTiming {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            stage: "validation",
            outcome: "error",
            read_ms: 0,
            embed_total_ms: 0,
            index_ms: 0,
            repo_save_ms: 0,
        }
    }

    fn completed(&mut self) {
        self.stage = "completed";
        self.outcome = "success";
    }
}

impl Drop for EditStageTiming {
    fn drop(&mut self) {
        info!(
            outcome = self.outcome,
            stage = self.stage,
            edit_total_ms = elapsed_ms(self.start),
            read_ms = self.read_ms,
            embed_total_ms = self.embed_total_ms,
            index_ms = self.index_ms,
            repo_save_ms = self.repo_save_ms,
            "edit stage timing"
        );
    }
}

// ---------------------------------------------------------------------------
// Incremental reindex helper
// ---------------------------------------------------------------------------

/// Re-embed and re-index all memories that changed between two commits.
///
/// Removals are processed first so a name that was deleted and re-added in
/// the same pull gets a fresh entry rather than a ghost. Lexical mutations
/// are accumulated and applied as one batch (a single Tantivy commit and
/// reader reload on the blocking pool), not one commit per memory.
async fn incremental_reindex(
    repo: &Arc<MemoryRepo>,
    embedding: &dyn EmbeddingBackend,
    index: &dyn VectorStore,
    lexical: &Arc<LexicalIndex>,
    changes: &ChangedMemories,
) -> ReindexStats {
    let mut stats = ReindexStats::default();
    let mut lexical_ops: Vec<LexicalOp> = Vec::new();

    // ---- 1. Removals --------------------------------------------------------
    for name in &changes.removed {
        match parse_qualified_name(name) {
            Ok(mref) => {
                let canonical = mref.qualified_path();
                lexical_ops.push(LexicalOp::Remove(canonical.clone()));
                match index.remove(&mref.scope, &canonical) {
                    Ok(()) => {
                        stats.removed += 1;
                    }
                    Err(e) => {
                        // For on-disk path keys with multi-segment hierarchical scopes,
                        // parse_qualified_name splits at the first slash, producing the
                        // wrong scope/name split. The canonical key computed here will
                        // then not match the actual index entry, and the remove is a
                        // no-op. This is acceptable: hierarchical scopes are new in this
                        // release, so no existing index entries use the legacy on-disk
                        // path form with multi-segment scopes. A full reindex resolves
                        // any stale entries if they exist.
                        let is_multi_segment_legacy = name.starts_with("projects/")
                            && name
                                .strip_prefix("projects/")
                                .map(|rest| rest.matches('/').count() >= 2)
                                .unwrap_or(false);
                        if is_multi_segment_legacy {
                            warn!(
                                qualified_name = %name,
                                canonical = %canonical,
                                error = %e,
                                "incremental_reindex: removal of multi-segment legacy path key \
                                 failed (scope ambiguity); a full reindex may be needed"
                            );
                        } else {
                            warn!(
                                qualified_name = %name,
                                error = %e,
                                "incremental_reindex: failed to remove vector; skipping"
                            );
                            stats.errors += 1;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    qualified_name = %name,
                    error = %e,
                    "incremental_reindex: cannot parse qualified name for removal; skipping"
                );
                // If we can't parse the name, we can't look it up — not an indexing error.
            }
        }
        // If not in index, remove is a no-op — not an error.
    }

    // ---- 2. Resolve MemoryRefs for upserts ----------------------------------
    let mut refs: Vec<MemoryRef> = Vec::new();
    for qualified in &changes.upserted {
        match parse_qualified_name(qualified) {
            Ok(mref) => refs.push(mref),
            Err(e) => {
                warn!(
                    qualified_name = %qualified,
                    error = %e,
                    "incremental_reindex: cannot parse qualified name; skipping"
                );
                stats.errors += 1;
            }
        }
    }

    // ---- 3. Read memories from disk -----------------------------------------
    // (MemoryRef, content)
    let mut to_embed: Vec<(MemoryRef, String)> = Vec::new();
    for mref in &refs {
        let qualified = mref.qualified_path();
        let memory = match repo.read_memory(&mref.name, &mref.scope).await {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    qualified_name = %qualified,
                    error = %e,
                    "incremental_reindex: failed to read memory; skipping"
                );
                stats.errors += 1;
                continue;
            }
        };
        lexical_ops.push(LexicalOp::Upsert(LexicalDoc {
            qualified_name: qualified,
            name: memory.name.as_str().to_string(),
            content: memory.content.clone(),
        }));
        to_embed.push((mref.clone(), memory.content));
    }

    // ---- 3b. Mirror all changes into the lexical index ----------------------
    // One batch: a single commit and reader reload for the whole changed
    // set, on the blocking pool. Best-effort — a failure flags the index
    // degraded (semantic-only recall) and repair rebuilds from git truth.
    if let Err(e) = lexical.apply_async(lexical_ops).await {
        warn!(
            error = %e,
            "incremental_reindex: lexical batch update failed; keyword search \
             degraded until repair completes"
        );
        spawn_lexical_repair(repo, lexical);
    }

    if to_embed.is_empty() {
        return stats;
    }

    // ---- 4. Batch embed all content -----------------------------------------
    let contents: Vec<String> = to_embed.iter().map(|(_, c)| c.clone()).collect();
    let vectors = match embedding.embed(&contents).await {
        Ok(v) => v,
        Err(batch_err) => {
            warn!(error = %batch_err, "incremental_reindex: batch embed failed; falling back to per-item");
            let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(contents.len());
            let mut failed: Vec<usize> = Vec::new();
            for (i, content) in contents.iter().enumerate() {
                match embedding.embed(std::slice::from_ref(content)).await {
                    Ok(mut v) => vecs.push(v.remove(0)),
                    Err(e) => {
                        warn!(
                            error = %e,
                            qualified_name = %to_embed[i].0.qualified_path(),
                            "incremental_reindex: per-item embed failed; skipping"
                        );
                        failed.push(i);
                        stats.errors += 1;
                    }
                }
            }
            // Remove failed items from to_embed in reverse order to preserve indices.
            for &i in failed.iter().rev() {
                to_embed.remove(i);
            }
            vecs
        }
    };

    // ---- 5. Update index entries --------------------------------------------
    for ((mref, _), vector) in to_embed.iter().zip(vectors.iter()) {
        let qualified_name = mref.qualified_path();
        let is_update = index.find_by_name(&qualified_name).is_some();

        match index.add(&mref.scope, vector, qualified_name.clone()) {
            Ok(_) => {}
            Err(e) => {
                warn!(
                    qualified_name = %qualified_name,
                    error = %e,
                    "incremental_reindex: add failed; skipping"
                );
                stats.errors += 1;
                continue;
            }
        }

        if is_update {
            stats.updated += 1;
        } else {
            stats.added += 1;
        }
    }

    stats
}

/// Re-embed and re-index all memories in the repository.
///
/// This is a full rebuild: all memories are listed, their content is embedded,
/// and the index is updated. Intended for startup freshness checks and
/// recovery after a crash that discarded an in-progress index.
///
/// Unlike delegating to `incremental_reindex`, this function uses the content
/// already loaded by `list_memories` to avoid reading each file a second time.
pub async fn full_reindex(
    repo: &Arc<MemoryRepo>,
    embedding: &dyn EmbeddingBackend,
    index: &dyn VectorStore,
) -> Result<ReindexStats, MemoryError> {
    let memories = repo.list_memories(None).await?;
    if memories.is_empty() {
        return Ok(ReindexStats::default());
    }

    let mut stats = ReindexStats::default();

    let items: Vec<(MemoryRef, String)> = memories
        .into_iter()
        .map(|m| {
            let mref = MemoryRef::new(m.metadata.scope, m.name);
            (mref, m.content)
        })
        .collect();

    // Embed and index in chunks so each embed() call maps to roughly one
    // BERT forward pass (MAX_BATCH_SIZE=64 inside the worker) and stays
    // within the per-call timeout budget.
    const REINDEX_BATCH_SIZE: usize = 64;
    for chunk in items.chunks(REINDEX_BATCH_SIZE) {
        let contents: Vec<String> = chunk.iter().map(|(_, c)| c.clone()).collect();

        let vectors = match embedding.embed(&contents).await {
            Ok(v) => v,
            Err(batch_err) => {
                warn!(error = %batch_err, "full_reindex: batch embed failed; falling back to per-item");
                let mut vecs = Vec::with_capacity(contents.len());
                for (i, content) in contents.iter().enumerate() {
                    match embedding.embed(std::slice::from_ref(content)).await {
                        Ok(mut v) => vecs.push(v.remove(0)),
                        Err(e) => {
                            warn!(
                                error = %e,
                                qualified_name = %chunk[i].0.qualified_path(),
                                "full_reindex: per-item embed failed; skipping"
                            );
                            stats.errors += 1;
                            vecs.push(Vec::new());
                        }
                    }
                }
                vecs
            }
        };

        debug_assert_eq!(
            vectors.len(),
            chunk.len(),
            "embed() must return exactly one vector per input"
        );

        for ((mref, _), vector) in chunk.iter().zip(vectors.iter()) {
            if vector.is_empty() {
                continue;
            }
            let qualified_name = mref.qualified_path();
            match index.add(&mref.scope, vector, qualified_name.clone()) {
                Ok(_) => stats.added += 1,
                Err(e) => {
                    warn!(
                        qualified_name = %qualified_name,
                        error = %e,
                        "full_reindex: index add failed; skipping"
                    );
                    stats.errors += 1;
                }
            }
        }
    }

    Ok(stats)
}

#[tool_router]
impl MemoryServer {
    /// Create a new MCP server backed by the given application state.
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Store a new memory in the git-backed repository.
    ///
    /// Writes `<scope>/<name>.md` with YAML frontmatter, commits to git,
    /// and indexes the content for semantic retrieval.
    ///
    /// Returns the assigned memory ID on success.
    #[tool(
        name = "remember",
        description = "Store a new memory. Saves the content to the git-backed repository and \
        indexes it for semantic search. Use scope '<basename-of-your-cwd>' or 'org/team' for \
        scoped memories, or omit for global. Returns the assigned memory ID. \
        IMPORTANT: Never store credentials, API keys, tokens, passwords, or other secrets — \
        memories are plaintext files in a git repo and may be synced to a remote."
    )]
    async fn remember(
        &self,
        Parameters(args): Parameters<RememberArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let name = MemoryName::new(args.name).map_err(ErrorData::from)?;
        if args.content.len() > MAX_CONTENT_SIZE {
            return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                reason: format!(
                    "content size {} exceeds maximum of {} bytes",
                    args.content.len(),
                    MAX_CONTENT_SIZE
                ),
            }));
        }
        let session_id = extract_session_id(&parts);
        let content_size = args.content.len();
        let span = tracing::info_span!(
            "handler.remember",
            session_id = %session_id,
            name = %name,
            scope = ?args.scope,
            content_size,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = Scope::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;
            let metadata = MemoryMetadata::new(scope.clone(), args.tags, args.source);
            let memory = Memory::from_validated(name, args.content, metadata);

            // Order: (1) embed, (2) add to index, (3) save to repo.
            // If step 3 fails, index has a stale entry (harmless — recall will skip it).
            // If step 1 or 2 fail, no repo commit happens.
            let start = Instant::now();
            let vector = state
                .embedding
                .embed_one(&memory.content)
                .await
                .map_err(ErrorData::from)?;
            info!(embed_ms = start.elapsed().as_millis(), "embedded");

            let qualified_name = memory.mem_ref().qualified_path();

            state
                .index
                .add(&scope, &vector, qualified_name.clone())
                .map_err(ErrorData::from)?;

            let start = Instant::now();
            state
                .repo
                .save_memory(&memory)
                .await
                .map_err(ErrorData::from)?;
            info!(repo_ms = start.elapsed().as_millis(), "saved to repo");

            // Mirror into the lexical index after git truth is durable, so a
            // save failure never leaves the index ahead of the repo. The
            // batch is dispatched eagerly (no await point between the save
            // and the dispatch), so request cancellation cannot strand a
            // saved memory unmirrored. Best-effort: a failure flags the
            // index degraded and repair rebuilds it from git truth.
            if let Err(e) = state
                .lexical
                .apply_async(vec![LexicalOp::Upsert(LexicalDoc {
                    qualified_name: qualified_name.clone(),
                    name: memory.name.as_str().to_string(),
                    content: memory.content.clone(),
                })])
                .await
            {
                warn!(
                    name = %memory.name,
                    error = %e,
                    "lexical index update failed during remember; keyword search \
                     degraded until repair completes"
                );
                spawn_lexical_repair(&state.repo, &state.lexical);
            }

            Ok(serde_json::json!({
                "id": memory.id,
                "name": memory.name,
                "scope": memory.metadata.scope.to_string(),
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Search memories by semantic similarity to a natural-language query.
    ///
    /// Embeds the query, searches the HNSW index, and returns the top-k
    /// most relevant memories with their names, scopes, and content snippets.
    ///
    /// Returns a JSON array of matching memories sorted by relevance.
    #[tool(
        name = "recall",
        description = "Search memories with hybrid retrieval: semantic similarity (embeddings) and keyword/BM25 \
        search run in parallel and are rank-fused, so exact phrases buried in long memories still surface. \
        Returns the top matching memories as a JSON array with name, scope, tags, and a content snippet (max 500 chars).\n\n\
        Each result includes `truncated` (bool) and `content_length` (total character count). \
        When `truncated` is true, the snippet is incomplete — use the `read` tool with the memory's name and scope \
        to retrieve the full content before acting on it.\n\n\
        Each result also includes `match_type` ('semantic', 'lexical', or 'both') and `distance` \
        (cosine distance, lower is more similar; always numeric — lexical-only hits, which have no \
        embedding distance, carry the sentinel -1.0).\n\n\
        Scope: pass '<basename-of-your-cwd>' or 'org/team' to search that scope + global memories, \
        'global' for global-only, or 'all' to search everything. Omitting scope defaults to global-only."
    )]
    async fn recall(
        &self,
        Parameters(args): Parameters<RecallArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let session_id = extract_session_id(&parts);
        let recall_id = RecallLog::generate_recall_id();
        // Note: query text is intentionally omitted from the span (R-17 privacy decision).
        let span = tracing::info_span!(
            "handler.recall",
            session_id = %session_id,
            recall_id = %recall_id,
            scope = ?args.scope,
            limit = ?args.limit,
            count = tracing::field::Empty,
        );
        let state = Arc::clone(&self.state);
        async move {
            // Parse optional scope filter.
            let scope_filter =
                ScopeFilter::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;

            let limit = args.limit.unwrap_or(5).min(100);

            // Degraded lexical index self-heals on read: kick a background
            // rebuild from git truth (single-flight). This query — and every
            // query until the rebuild converges — serves semantic-only.
            if state.lexical.is_degraded() {
                spawn_lexical_repair(&state.repo, &state.lexical);
            }

            // Semantic and lexical retrieval run in parallel; ranked lists
            // are merged with reciprocal rank fusion so an exact keyword hit
            // can surface even when its embedding distance is poor.
            let start = Instant::now();
            let fused = hybrid_search(
                state.embedding.as_ref(),
                state.index.as_ref(),
                &state.lexical,
                &scope_filter,
                &args.query,
                limit,
            )
            .await
            .map_err(ErrorData::from)?;
            info!(
                search_ms = start.elapsed().as_millis(),
                candidates = fused.len(),
                "hybrid search complete"
            );

            let pre_filter_count = fused.len();
            let mut results_vec = Vec::new();
            let mut log_entries: Vec<RecallResult> = Vec::new();
            let mut skipped_errors: usize = 0;

            for hit in fused {
                // Fusion returns at most `limit` candidates; this guard is a safety
                // net that only activates if more candidates arrive than expected.
                if results_vec.len() >= limit {
                    break;
                }
                let mref = match parse_qualified_name(&hit.qualified_name) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            qualified_name = %hit.qualified_name,
                            error = %e,
                            "could not parse qualified name from index; skipping"
                        );
                        skipped_errors += 1;
                        continue;
                    }
                };

                // Read the memory; if it was deleted but still in the index, skip it.
                let memory = match state.repo.read_memory(&mref.name, &mref.scope).await {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(
                            name = %mref.name,
                            error = %e,
                            "could not read memory from repo (deleted?); skipping"
                        );
                        skipped_errors += 1;
                        continue;
                    }
                };

                let rank = results_vec.len();

                log_entries.push(RecallResult {
                    memory_name: memory.name.to_string(),
                    scope: memory.metadata.scope.to_string(),
                    rank,
                    // Lexical-only hits have no embedding distance; the -1.0
                    // sentinel keeps them out of distance-bucketed recall
                    // stats (which filter on distance >= 0.0).
                    distance: hit
                        .semantic_distance
                        .map_or(LEXICAL_ONLY_DISTANCE_SENTINEL, f64::from),
                });

                results_vec.push(recall_entry_json(&memory, &hit));
            }

            if let Some(ref log) = state.recall_log {
                if let Err(e) = log.log_results(&recall_id, &session_id, &log_entries) {
                    warn!(error = %e, "failed to write recall log entries");
                }
            }

            let count = results_vec.len();
            tracing::Span::current().record("count", count);
            info!(returned = count, skipped_errors, "recall complete");

            Ok(serde_json::json!({
                "recall_id": recall_id,
                "results": results_vec,
                "count": count,
                "limit": limit,
                "pre_filter_count": pre_filter_count,
                "skipped_errors": skipped_errors,
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Delete a memory from the repository and vector index.
    ///
    /// Removes the file from the git working tree, commits the deletion,
    /// and removes the corresponding vector from the HNSW index.
    ///
    /// Returns `"ok"` on success.
    #[tool(
        name = "forget",
        description = "Delete a memory by name. Use a bare path scope like '<basename-of-your-cwd>' for scoped \
        memories or omit for global. Removes the file from git and the vector from the search index. \
        Returns 'ok' on success."
    )]
    async fn forget(
        &self,
        Parameters(args): Parameters<ForgetArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let name = MemoryName::new(args.name).map_err(ErrorData::from)?;
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.forget",
            session_id = %session_id,
            name = %name,
            scope = ?args.scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = Scope::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();

            // Delete from repo first — if this fails, index is untouched, memory stays functional.
            state
                .repo
                .delete_memory(&name, &scope)
                .await
                .map_err(ErrorData::from)?;

            // Remove from index (best-effort — stale entries are skipped at recall time).
            let qualified_name = MemoryRef::new(scope.clone(), name.clone()).qualified_path();
            if let Err(e) = state.index.remove(&scope, &qualified_name) {
                warn!(name = %name, error = %e, "vector removal failed during forget; stale entry will be skipped at recall");
            }
            if let Err(e) = state
                .lexical
                .apply_async(vec![LexicalOp::Remove(qualified_name.clone())])
                .await
            {
                warn!(name = %name, error = %e, "lexical removal failed during forget; keyword search degraded until repair completes");
                spawn_lexical_repair(&state.repo, &state.lexical);
            }

            info!(
                ms = start.elapsed().as_millis(),
                name = %name,
                "memory forgotten"
            );

            Ok("ok".to_string())
        }
        .instrument(span)
        .await
    }

    /// Update the content or tags of an existing memory.
    ///
    /// Supports partial updates: omit `content` to keep the existing body,
    /// omit `tags` to keep the existing tags. The `updated_at` timestamp is
    /// refreshed, the change is committed to git, and the vector index is
    /// updated with a fresh embedding.
    ///
    /// Returns the updated memory ID.
    #[tool(
        name = "edit",
        description = "Edit an existing memory. Supports partial updates — omit content or \
        tags to preserve existing values. Re-embeds and re-indexes the memory. Use a bare path \
        scope like '<basename-of-your-cwd>' for scoped memories. Returns the memory ID. \
        IMPORTANT: Never store credentials, API keys, tokens, passwords, or other secrets — \
        memories are plaintext files in a git repo and may be synced to a remote."
    )]
    async fn edit(
        &self,
        Parameters(args): Parameters<EditArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let mut timing = EditStageTiming::new();
        let name = MemoryName::new(args.name).map_err(ErrorData::from)?;
        if args.content.is_none() && args.tags.is_none() {
            return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                reason: "nothing to update — provide content or tags".into(),
            }));
        }
        if let Some(ref content) = args.content {
            if content.len() > MAX_CONTENT_SIZE {
                return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                    reason: format!(
                        "content size {} exceeds maximum of {} bytes",
                        content.len(),
                        MAX_CONTENT_SIZE
                    ),
                }));
            }
        }
        let session_id = extract_session_id(&parts);
        let content_size = args.content.as_ref().map(|c| c.len()).unwrap_or(0);
        let span = tracing::info_span!(
            "handler.edit",
            session_id = %session_id,
            name = %name,
            scope = ?args.scope,
            content_size,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = Scope::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;

            // Track whether content changed so we can skip re-embedding when only tags changed.
            let content_changed = args.content.is_some();

            // Read the existing memory.
            timing.stage = "read";
            let stage_start = Instant::now();
            let read_result = state.repo.read_memory(&name, &scope).await;
            timing.read_ms = elapsed_ms(stage_start);
            let mut memory = read_result.map_err(ErrorData::from)?;

            // Apply partial updates.
            if let Some(content) = args.content {
                memory.content = content;
            }
            if let Some(tags) = args.tags {
                memory.metadata.tags = tags;
            }
            memory.metadata.updated_at = Utc::now();

            // Only re-embed and re-index when content changed.
            // Order: (1) embed, (2) upsert index entry, (3) save to repo.
            if content_changed {
                let qualified_name = memory.mem_ref().qualified_path();

                // Re-embed updated content.
                timing.stage = "embed";
                let stage_start = Instant::now();
                let embed_result = state.embedding.embed_one(&memory.content).await;
                timing.embed_total_ms = elapsed_ms(stage_start);
                let vector = embed_result.map_err(ErrorData::from)?;

                timing.stage = "index";
                let stage_start = Instant::now();
                let index_result = state.index.add(&scope, &vector, qualified_name.clone());
                timing.index_ms = elapsed_ms(stage_start);
                index_result.map_err(ErrorData::from)?;
            }

            // Persist to repo (last, so partial failures leave recoverable state).
            timing.stage = "repo_save";
            let stage_start = Instant::now();
            let save_result = state.repo.save_memory(&memory).await;
            timing.repo_save_ms = elapsed_ms(stage_start);
            save_result.map_err(ErrorData::from)?;

            // Mirror into the lexical index after git truth is durable, so a
            // save failure never leaves the index serving content the repo
            // does not hold. Eager dispatch: no await point between the save
            // and the dispatch, so cancellation cannot strand the mirror.
            // Best-effort: a failure flags the index degraded and repair
            // rebuilds it from git truth.
            if content_changed {
                if let Err(e) = state
                    .lexical
                    .apply_async(vec![LexicalOp::Upsert(LexicalDoc {
                        qualified_name: memory.mem_ref().qualified_path(),
                        name: memory.name.as_str().to_string(),
                        content: memory.content.clone(),
                    })])
                    .await
                {
                    warn!(
                        name = %memory.name,
                        error = %e,
                        "lexical index update failed during edit; keyword search \
                         degraded until repair completes"
                    );
                    spawn_lexical_repair(&state.repo, &state.lexical);
                }
            }

            info!(
                ms = elapsed_ms(timing.start),
                name = %name,
                content_changed,
                "memory edited"
            );

            timing.completed();

            Ok(serde_json::json!({
                "id": memory.id,
                "name": memory.name,
                "scope": memory.metadata.scope.to_string(),
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Move a memory from one scope to another, optionally renaming it.
    ///
    /// Reads the source memory, creates it in the destination scope (with a
    /// new name if provided), re-indexes the embedding, then deletes the
    /// source. Atomic: if the create fails, the source is preserved.
    ///
    /// Returns the new memory ID, name, and scope on success.
    #[tool(
        name = "move",
        description = "Move a memory from one scope to another, optionally renaming it. \
        Reads the source memory (content, tags, metadata), creates it in the destination scope, \
        re-indexes the embedding, and deletes the original. Atomic: if the create fails, the \
        source is preserved. Use 'new_name' to rename during the move. Returns the new memory \
        ID, name, and scope."
    )]
    async fn move_memory(
        &self,
        Parameters(args): Parameters<MoveArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let name = MemoryName::new(args.name).map_err(ErrorData::from)?;
        let new_name = match args.new_name {
            Some(n) => MemoryName::new(n).map_err(ErrorData::from)?,
            None => name.clone(),
        };
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.move",
            session_id = %session_id,
            name = %name,
            new_name = %new_name,
            from_scope = ?args.from_scope,
            to_scope = %args.to_scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let from_scope =
                Scope::parse_or_default(args.from_scope.as_deref()).map_err(ErrorData::from)?;
            let to_scope =
                Scope::parse_or_default(Some(&args.to_scope)).map_err(ErrorData::from)?;

            // Reject no-op moves (same scope + same name).
            if from_scope == to_scope && name == new_name {
                return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                    reason: "source and destination are identical — nothing to move".into(),
                }));
            }

            let start = Instant::now();

            // 1. Reject early if the destination already exists.
            match state.repo.read_memory(&new_name, &to_scope).await {
                Ok(_) => {
                    return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                        reason: format!(
                            "destination memory '{}' already exists in scope '{}' — \
                             rename or delete it first",
                            new_name, to_scope
                        ),
                    }));
                }
                Err(crate::error::MemoryError::NotFound { .. }) => {
                    // Good — destination is clear.
                }
                Err(e) => {
                    return Err(ErrorData::from(e));
                }
            }

            // 2. Atomically read source, write destination, delete source
            //    in one git commit. Must happen before index mutations so a
            //    failure leaves the index consistent with the repo on disk.
            let dest = state
                .repo
                .move_memory(&name, &from_scope, &new_name, &to_scope)
                .await
                .map_err(ErrorData::from)?;

            // 3. Embed the content for the new scope's index entry.
            let vector = state
                .embedding
                .embed_one(&dest.content)
                .await
                .map_err(ErrorData::from)?;

            let dest_qualified = dest.mem_ref().qualified_path();

            // 4. Add destination to the vector index.
            state
                .index
                .add(&to_scope, &vector, dest_qualified.clone())
                .map_err(ErrorData::from)?;

            // 5. Remove the source from the vector index (best-effort — stale
            //    entries are skipped at recall time).
            let source_qualified =
                MemoryRef::new(from_scope.clone(), name.clone()).qualified_path();
            if let Err(e) = state.index.remove(&from_scope, &source_qualified) {
                warn!(
                    name = %name,
                    error = %e,
                    "vector removal failed during move; stale source entry will be skipped at recall"
                );
            }

            // 6. Mirror the move in the lexical index (best-effort). One
            //    batch: destination upsert + source removal share a single
            //    commit and reader reload on the blocking pool.
            if let Err(e) = state
                .lexical
                .apply_async(vec![
                    LexicalOp::Upsert(LexicalDoc {
                        qualified_name: dest_qualified.clone(),
                        name: dest.name.as_str().to_string(),
                        content: dest.content.clone(),
                    }),
                    LexicalOp::Remove(source_qualified.clone()),
                ])
                .await
            {
                warn!(
                    name = %name,
                    new_name = %new_name,
                    error = %e,
                    "lexical index update failed during move; keyword search degraded \
                     until repair completes"
                );
                spawn_lexical_repair(&state.repo, &state.lexical);
            }

            info!(
                ms = start.elapsed().as_millis(),
                name = %name,
                new_name = %new_name,
                from_scope = %from_scope,
                to_scope = %to_scope,
                "memory moved"
            );

            Ok(serde_json::json!({
                "id": dest.id,
                "name": dest.name,
                "scope": dest.metadata.scope.to_string(),
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// List stored memories, optionally filtered by scope.
    ///
    /// Returns a JSON array of memory summaries (id, name, scope, tags,
    /// created_at, updated_at). Full content bodies are omitted for brevity.
    #[tool(
        name = "list",
        description = "List stored memories. Pass a bare path like '<basename-of-your-cwd>' for that scope + global memories, \
        'global' for global-only, or 'all' for everything. Omitting scope defaults to global-only. \
        Returns a JSON array of memory summaries without full content."
    )]
    async fn list(
        &self,
        Parameters(args): Parameters<ListArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.list",
            session_id = %session_id,
            scope = ?args.scope,
            count = tracing::field::Empty,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope_filter =
                ScopeFilter::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();
            let memories = match &scope_filter {
                ScopeFilter::RootOnly => state
                    .repo
                    .list_memories(Some(&Scope::Root))
                    .await
                    .map_err(ErrorData::from)?,
                ScopeFilter::All => state
                    .repo
                    .list_memories(None)
                    .await
                    .map_err(ErrorData::from)?,
                ScopeFilter::Subtree(sp) => {
                    let path_scope = Scope::Path(sp.clone());
                    let mut root_memories = state
                        .repo
                        .list_memories(Some(&Scope::Root))
                        .await
                        .map_err(ErrorData::from)?;
                    let path_memories = state
                        .repo
                        .list_memories(Some(&path_scope))
                        .await
                        .map_err(ErrorData::from)?;
                    root_memories.extend(path_memories);
                    root_memories
                }
            };
            let count = memories.len();
            tracing::Span::current().record("count", count);
            info!(ms = start.elapsed().as_millis(), count, "listed memories");

            let summaries: Vec<serde_json::Value> = memories
                .into_iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "name": m.name,
                        "scope": m.metadata.scope.to_string(),
                        "tags": m.metadata.tags,
                        "created_at": m.metadata.created_at,
                        "updated_at": m.metadata.updated_at,
                    })
                })
                .collect();

            Ok(serde_json::json!({
                "memories": summaries,
                "count": count,
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Read the full content of a specific memory by name.
    ///
    /// Returns the memory's markdown content (frontmatter stripped) plus
    /// metadata (id, scope, tags, timestamps) as a JSON object.
    #[tool(
        name = "read",
        description = "Read a specific memory by name. Use a bare path scope like '<basename-of-your-cwd>' for \
        scoped memories or omit for global. Returns the full markdown content and metadata \
        (id, scope, tags, timestamps) as a JSON object."
    )]
    async fn read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let name = MemoryName::new(args.name).map_err(ErrorData::from)?;
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.read",
            session_id = %session_id,
            name = %name,
            scope = ?args.scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = Scope::parse_or_default(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();
            let memory = state
                .repo
                .read_memory(&name, &scope)
                .await
                .map_err(ErrorData::from)?;
            info!(
                ms = start.elapsed().as_millis(),
                name = %name,
                "read memory"
            );

            Ok(serde_json::json!({
                "id": memory.id,
                "name": memory.name,
                "scope": memory.metadata.scope.to_string(),
                "tags": memory.metadata.tags,
                "content": memory.content,
                "source": memory.metadata.source,
                "created_at": memory.metadata.created_at,
                "updated_at": memory.metadata.updated_at,
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Synchronise the memory repository with the configured git remote.
    ///
    /// Optionally pulls before pushing (default: true). Requires a GitHub
    /// token configured via `MEMORY_MCP_GITHUB_TOKEN` or
    /// `~/.config/memory-mcp/token`.
    ///
    /// Returns a status message describing what happened.
    #[tool(
        name = "sync",
        description = "Sync the memory repo with the git remote (push/pull). Requires \
        MEMORY_MCP_GITHUB_TOKEN or a token file. Returns a status message."
    )]
    async fn sync(
        &self,
        Parameters(args): Parameters<SyncArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let pull_first = args.pull_first.unwrap_or(true);
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.sync",
            session_id = %session_id,
            pull_first,
        );
        let state = Arc::clone(&self.state);
        async move {
            let start = Instant::now();
            let branch = &state.branch;

            // Track whether origin is configured at all so we can skip push
            // for local-only deployments that have no remote.
            let mut has_remote = true;

            let mut reindex_stats: Option<ReindexStats> = None;

            let pull_status = if pull_first {
                let result = state
                    .repo
                    .pull(&state.auth, branch)
                    .await
                    .map_err(ErrorData::from)?;

                let mut oid_range: Option<([u8; 20], [u8; 20])> = None;
                let status = match result {
                    PullResult::NoRemote => {
                        has_remote = false;
                        "no-remote".to_string()
                    }
                    PullResult::UpToDate => "up-to-date".to_string(),
                    PullResult::FastForward { old_head, new_head } => {
                        oid_range = Some((old_head, new_head));
                        "fast-forward".to_string()
                    }
                    PullResult::Merged {
                        conflicts_resolved,
                        old_head,
                        new_head,
                    } => {
                        oid_range = Some((old_head, new_head));
                        format!("merged ({} conflicts resolved)", conflicts_resolved)
                    }
                };

                if let Some((old_head, new_head)) = oid_range {
                    let repo = Arc::clone(&state.repo);
                    let changes = crate::repo::traced_spawn_blocking(move || {
                        repo.diff_changed_memories(old_head, new_head)
                    })
                    .await
                    .map_err(|e| MemoryError::Join(e.to_string()))
                    .map_err(ErrorData::from)?
                    .map_err(ErrorData::from)?;

                    let mut reindex_failed_completely = false;
                    if !changes.is_empty() {
                        let stats = incremental_reindex(
                            &state.repo,
                            state.embedding.as_ref(),
                            state.index.as_ref(),
                            &state.lexical,
                            &changes,
                        )
                        .instrument(tracing::info_span!("server.incremental_reindex"))
                        .await;
                        info!(
                            added = stats.added,
                            updated = stats.updated,
                            removed = stats.removed,
                            errors = stats.errors,
                            "incremental reindex complete"
                        );
                        reindex_failed_completely =
                            stats.added == 0 && stats.updated == 0 && stats.errors > 0;
                        reindex_stats = Some(stats);
                    }

                    // Advance the stored SHA so the next startup doesn't trigger
                    // a full reindex for changes already processed. Skip when every
                    // embed failed so the next startup retries.
                    if !reindex_failed_completely {
                        if let Some(sha) = state.repo.head_sha().await {
                            state.index.set_commit_sha(Some(&sha));
                        }
                    }
                }

                status
            } else {
                "skipped".to_string()
            };

            if has_remote {
                state
                    .repo
                    .push(&state.auth, branch)
                    .await
                    .map_err(ErrorData::from)?;
            }

            info!(
                ms = start.elapsed().as_millis(),
                pull_first,
                pull_status = %pull_status,
                "sync complete"
            );

            let mut response = serde_json::json!({
                "status": "sync complete",
                "pull": pull_status,
                "branch": branch,
            });

            if let Some(stats) = reindex_stats {
                response["reindex"] = serde_json::json!({
                    "added": stats.added,
                    "updated": stats.updated,
                    "removed": stats.removed,
                    "errors": stats.errors,
                });
            }

            Ok(response.to_string())
        }
        .instrument(span)
        .await
    }

    /// Return recall precision statistics bucketed by distance range.
    ///
    /// Reads all rows in the recall event log and groups them into 0.05-wide
    /// distance buckets. Each bucket reports applied / maybe / not_applied /
    /// unknown counts, which agents can use to calibrate their recall distance
    /// Report that a recalled memory influenced this session.
    ///
    /// Writes the agent's feedback back to the recall log — whether the memory
    /// was applied, an optional note about how it was used, and a confidence
    /// level. This data feeds into threshold calibration via `recall-stats`.
    ///
    /// Returns a JSON object with `rows_affected`.
    #[tool(
        name = "mark_applied",
        description = "Report whether a recalled memory was useful. Call for each memory in the \
        recall results after you have decided whether to use it. Pass verdict='applied' if it \
        influenced the session, 'maybe' if partially relevant, or 'not_applied' if not relevant. \
        This bidirectional feedback calibrates recall thresholds. Pass the recall_id from the \
        recall response, the memory name, the verdict, an optional application note, and a \
        confidence level ('high', 'medium', or 'low')."
    )]
    async fn mark_applied(
        &self,
        Parameters(args): Parameters<MarkAppliedArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.mark_applied",
            session_id = %session_id,
            recall_id = %args.recall_id,
            memory = %args.memory,
            verdict = %args.verdict,
        );
        let state = Arc::clone(&self.state);
        async move {
            match args.confidence.as_str() {
                "high" | "medium" | "low" => {}
                other => {
                    return Err(ErrorData::from(MemoryError::InvalidInput {
                        reason: format!(
                            "invalid confidence '{}'; expected 'high', 'medium', or 'low'",
                            other
                        ),
                    }));
                }
            }

            let rows_affected = if let Some(ref log) = state.recall_log {
                let log = Arc::clone(log);
                let recall_id = args.recall_id.clone();
                let memory = args.memory.clone();
                let sid = session_id.clone();
                let verdict = args.verdict.as_str();
                let application = args.application.clone();
                let confidence = args.confidence.clone();
                traced_spawn_blocking(move || {
                    log.mark_applied(
                        &recall_id,
                        &memory,
                        &sid,
                        verdict,
                        application.as_deref(),
                        &confidence,
                    )
                })
                .await
                .map_err(|e| {
                    ErrorData::from(MemoryError::Internal(format!("spawn_blocking: {e}")))
                })?
                .map_err(ErrorData::from)?
            } else {
                warn!("mark_applied called but recall log is not enabled");
                0
            };

            if rows_affected == 0 {
                warn!(
                    recall_id = %args.recall_id,
                    memory = %args.memory,
                    "mark_applied matched no rows"
                );
            }

            info!(
                rows_affected,
                memory = %args.memory,
                verdict = %args.verdict,
                "mark_applied"
            );

            Ok(serde_json::json!({
                "rows_affected": rows_affected,
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Report verdicts for multiple recalled memories in a single call.
    ///
    /// Accepts an array of verdict entries and processes them in one database
    /// transaction. Each entry follows the same semantics as `mark_applied`:
    /// first call wins per (`recall_id`, `memory_name`, `session_id`) tuple.
    ///
    /// Returns per-entry row counts and an aggregate total.
    #[tool(
        name = "batch_mark_applied",
        description = "Report verdicts for multiple recalled memories in a single call. Pass an \
        array of verdict objects, each with recall_id, memory name, verdict ('applied', 'maybe', \
        or 'not_applied'), optional application note, and confidence level. Processes all verdicts \
        in one transaction. Use this instead of calling mark_applied repeatedly — it reduces \
        round-trips from N calls to 1."
    )]
    async fn batch_mark_applied(
        &self,
        Parameters(args): Parameters<BatchMarkAppliedArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let session_id = extract_session_id(&parts);
        let count = args.verdicts.len();
        let span = tracing::info_span!(
            "handler.batch_mark_applied",
            session_id = %session_id,
            count,
        );
        let state = Arc::clone(&self.state);
        async move {
            if args.verdicts.is_empty() {
                return Err(ErrorData::from(MemoryError::InvalidInput {
                    reason: "verdicts array must not be empty".into(),
                }));
            }

            // Validate all confidence values up front.
            for (i, entry) in args.verdicts.iter().enumerate() {
                match entry.confidence.as_str() {
                    "high" | "medium" | "low" => {}
                    other => {
                        return Err(ErrorData::from(MemoryError::InvalidInput {
                            reason: format!(
                                "invalid confidence '{}' at index {}; expected 'high', 'medium', or 'low'",
                                other, i
                            ),
                        }));
                    }
                }
            }

            let total_rows = if let Some(ref log) = state.recall_log {
                let log = Arc::clone(log);
                let sid = session_id.clone();

                // Build owned copies directly from the deserialized args so we
                // can move them into spawn_blocking.
                let owned: Vec<OwnedBatchVerdict> = args
                    .verdicts
                    .iter()
                    .map(|v| OwnedBatchVerdict {
                        recall_id: v.recall_id.clone(),
                        memory_name: v.memory.clone(),
                        verdict: v.verdict.as_str().to_owned(),
                        application_note: v.application.clone(),
                        confidence: v.confidence.clone(),
                    })
                    .collect();

                let per_entry = traced_spawn_blocking(move || {
                    let refs: Vec<BatchVerdict<'_>> = owned
                        .iter()
                        .map(|o| BatchVerdict {
                            recall_id: &o.recall_id,
                            memory_name: &o.memory_name,
                            verdict: &o.verdict,
                            application_note: o.application_note.as_deref(),
                            confidence: &o.confidence,
                        })
                        .collect();
                    log.batch_mark_applied(&sid, &refs)
                })
                .await
                .map_err(|e| {
                    ErrorData::from(MemoryError::Internal(format!("spawn_blocking: {e}")))
                })?
                .map_err(ErrorData::from)?;

                let total: u64 = per_entry.iter().sum();

                // Warn for entries that matched nothing.
                for (i, &rows) in per_entry.iter().enumerate() {
                    if rows == 0 {
                        warn!(
                            recall_id = %args.verdicts[i].recall_id,
                            memory = %args.verdicts[i].memory,
                            index = i,
                            "batch_mark_applied entry matched no rows"
                        );
                    }
                }

                info!(
                    total_rows = total,
                    entries = per_entry.len(),
                    "batch_mark_applied"
                );

                total
            } else {
                warn!("batch_mark_applied called but recall log is not enabled");
                0
            };

            Ok(serde_json::json!({
                "total_rows_affected": total_rows,
                "entries_processed": args.verdicts.len(),
            })
            .to_string())
        }
        .instrument(span)
        .await
    }

    /// Return recall precision statistics bucketed by distance range.
    ///
    /// Agents can use this to inspect their own recall quality and adjust
    /// thresholds.
    ///
    /// Returns an error if the recall log is not enabled.
    #[tool(
        name = "recall_stats",
        description = "Return recall precision statistics bucketed by distance range. Shows applied/maybe/not_applied/unknown counts per bucket, useful for calibrating recall thresholds. No arguments required."
    )]
    async fn recall_stats(
        &self,
        Parameters(_args): Parameters<RecallStatsArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<String, ErrorData> {
        let session_id = extract_session_id(&parts);
        let span = tracing::info_span!(
            "handler.recall_stats",
            session_id = %session_id,
        );
        let state = Arc::clone(&self.state);
        async move {
            if state.recall_log.is_none() {
                return Err(ErrorData::from(MemoryError::Internal(
                    "recall log is not enabled".to_string(),
                )));
            }
            let buckets = crate::repo::traced_spawn_blocking(move || {
                // Safety: checked above that recall_log is Some.
                state
                    .recall_log
                    .as_ref()
                    .expect("recall_log is Some — checked above")
                    .recall_stats()
            })
            .await
            .map_err(|e| ErrorData::from(MemoryError::Internal(format!("spawn_blocking: {e}"))))?
            .map_err(ErrorData::from)?;

            let json_buckets: Vec<serde_json::Value> = buckets
                .iter()
                .filter(|b| b.total > 0)
                .map(|b| {
                    serde_json::json!({
                        "range": format!("{:.2}–{:.2}", b.range_start, b.range_end),
                        "total": b.total,
                        "applied": b.applied,
                        "maybe": b.maybe,
                        "not_applied": b.not_applied,
                        "unknown": b.unknown,
                    })
                })
                .collect();

            info!(buckets = json_buckets.len(), "recall_stats");

            Ok(serde_json::json!({
                "buckets": json_buckets,
            })
            .to_string())
        }
        .instrument(span)
        .await
    }
}

#[tool_handler]
impl ServerHandler for MemoryServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let start = Instant::now();
        let tool_name = request.name.to_string();
        let context = ToolCallContext::new(self, request, context);
        finish_tool_call(&tool_name, start, self.tool_router.call(context).await)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "A semantic memory system for AI coding agents. Memories are stored as markdown files \
            in a git repository and indexed for semantic retrieval. Use `remember` to store, `recall` \
            to search, `read` to fetch a specific memory, `edit` to update, `forget` to delete, \
            `list` to browse, and `sync` to push/pull the remote.\n\n\
            Scope convention: pass a bare namespace path like 'my-project' or 'org/team' when \
            working within a namespace. Paths may be hierarchical (e.g. 'org/team/project'). \
            This returns that namespace's memories alongside global ones. \
            Omitting scope defaults to global-only for queries (recall, list) and targets a single \
            memory for point operations (remember, edit, read, forget). Use scope='all' to search \
            across all scopes.\n\n\
            IMPORTANT: Never store credentials, API keys, tokens, passwords, or other secrets in \
            memory content. Memories are stored as plaintext markdown files committed to a git \
            repository and may be synced to a remote. Treat all memory content as public.\n\n\
            Recall feedback: after acting on recalled memories, call `mark_applied` for each result \
            with your verdict ('applied', 'maybe', or 'not_applied'). Every recall response includes \
            a `recall_id` — pass it back with each verdict. This bidirectional feedback calibrates \
            recall thresholds. Use `batch_mark_applied` to submit multiple verdicts in a single call \
            instead of calling `mark_applied` repeatedly. Use `recall_stats` to inspect precision \
            by distance bucket.\n\n\
            Breaking change (v0.14.0): the 'project:<name>' scope format is no longer accepted \
            as tool input. If your prompts or configuration pass scope values like \
            'project:my-project', replace them with the bare path: 'my-project'. The 'global' \
            scope is unchanged. Stored memories are unaffected — old scope formats in YAML \
            frontmatter are read correctly without migration."
                .to_string(),
        )
    }
}

/// Sentinel `distance` for hits that only the lexical strategy returned.
///
/// Lexical-only hits have no embedding distance, but the `distance` field
/// predates hybrid retrieval and was always numeric on the wire — strict
/// clients deserialize it as a required float, so it must stay numeric
/// (never `null` or absent). `-1.0` is impossible as a real cosine distance
/// and matches the recall log's lexical-only sentinel.
const LEXICAL_ONLY_DISTANCE_SENTINEL: f64 = -1.0;

/// Build one recall result entry as seen on the wire.
///
/// `distance` is the semantic (cosine) distance, or the numeric sentinel
/// [`LEXICAL_ONLY_DISTANCE_SENTINEL`] (`-1.0`) for hits that only the
/// lexical strategy returned. `match_type` says which strategies
/// contributed: `"semantic"`, `"lexical"`, or `"both"`.
fn recall_entry_json(memory: &Memory, hit: &FusedHit) -> serde_json::Value {
    let (snippet, content_length, truncated) = build_snippet(&memory.content);
    serde_json::json!({
        "id": memory.id,
        "name": memory.name,
        "scope": memory.metadata.scope.to_string(),
        "tags": memory.metadata.tags,
        "content": snippet,
        "content_length": content_length,
        "truncated": truncated,
        "distance": hit
            .semantic_distance
            .map_or(LEXICAL_ONLY_DISTANCE_SENTINEL, f64::from),
        "match_type": hit.match_type(),
    })
}

/// Truncate content to [`SNIPPET_MAX_CHARS`] and return `(snippet, content_length, truncated)`.
fn build_snippet(content: &str) -> (String, usize, bool) {
    let content_length = content.chars().count();
    let truncated = content_length > SNIPPET_MAX_CHARS;
    let snippet: String = content.chars().take(SNIPPET_MAX_CHARS).collect();
    (snippet, content_length, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Content;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };
    use tracing::subscriber::with_default;
    use tracing_subscriber::{layer::SubscriberExt, Registry};

    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_info_logs(f: impl FnOnce()) -> String {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer_output = Arc::clone(&output);
        let subscriber = Registry::default()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(move || TestWriter(Arc::clone(&writer_output))),
            );
        with_default(subscriber, f);
        let bytes = output.lock().expect("log buffer").clone();
        String::from_utf8(bytes).expect("UTF-8 logs")
    }

    #[test]
    fn tool_timing_preserves_content_and_adds_metadata() {
        let original = CallToolResult::success(vec![Content::text("{\"ok\":true}")]);
        let original_content = original.content.clone();

        let result = finish_tool_call("read", Instant::now(), Ok(original)).expect("success");

        assert_eq!(result.content, original_content);
        assert_eq!(result.structured_content, None);
        assert_eq!(result.is_error, Some(false));
        let duration = result
            .meta
            .as_ref()
            .and_then(|meta| meta.get(SERVER_PROCESSING_DURATION_META_KEY))
            .and_then(serde_json::Value::as_u64);
        assert!(duration.is_some());
    }

    #[test]
    fn tool_timing_merges_existing_metadata() {
        let mut meta = Meta::new();
        meta.insert("existing".to_string(), serde_json::json!("kept"));
        let original = CallToolResult::success(vec![Content::text("ok")]).with_meta(Some(meta));

        let result = finish_tool_call("list", Instant::now(), Ok(original)).expect("success");
        let meta = result.meta.expect("metadata");

        assert_eq!(meta.get("existing"), Some(&serde_json::json!("kept")));
        assert!(meta.contains_key(SERVER_PROCESSING_DURATION_META_KEY));
    }

    #[test]
    fn tool_timing_preserves_error_data_semantics() {
        let original =
            ErrorData::invalid_params("bad input", Some(serde_json::json!({"field": "name"})));
        let expected_code = original.code;
        let expected_message = original.message.clone();
        let expected_data = original.data.clone();

        let error = finish_tool_call("edit", Instant::now(), Err(original)).expect_err("error");

        assert_eq!(error.code, expected_code);
        assert_eq!(error.message, expected_message);
        assert_eq!(error.data, expected_data);
    }

    #[test]
    fn tool_timing_logs_success_and_error_at_info() {
        let logs = capture_info_logs(|| {
            let success = CallToolResult::success(vec![Content::text("ok")]);
            finish_tool_call("read", Instant::now(), Ok(success)).expect("success");
            let error = ErrorData::invalid_params("bad input", None);
            finish_tool_call("edit", Instant::now(), Err(error)).expect_err("error");
        });

        assert!(logs.contains("tool_name=\"read\""), "logs: {logs}");
        assert!(logs.contains("tool_name=\"edit\""), "logs: {logs}");
        assert!(logs.contains("outcome=\"success\""), "logs: {logs}");
        assert!(logs.contains("outcome=\"error\""), "logs: {logs}");
        assert!(
            logs.contains("server_processing_duration_ms="),
            "logs: {logs}"
        );
    }

    #[test]
    fn edit_stage_timing_logs_success_and_failure_at_info() {
        let logs = capture_info_logs(|| {
            let mut success = EditStageTiming::new();
            success.read_ms = 1;
            success.embed_total_ms = 2;
            success.index_ms = 3;
            success.repo_save_ms = 4;
            success.completed();
            drop(success);

            let mut failure = EditStageTiming::new();
            failure.stage = "embed";
            failure.read_ms = 1;
            failure.embed_total_ms = 2;
            drop(failure);
        });

        assert!(logs.contains("stage=\"completed\""), "logs: {logs}");
        assert!(logs.contains("stage=\"embed\""), "logs: {logs}");
        assert!(logs.contains("outcome=\"success\""), "logs: {logs}");
        assert!(logs.contains("outcome=\"error\""), "logs: {logs}");
        for field in [
            "edit_total_ms=",
            "read_ms=",
            "embed_total_ms=",
            "index_ms=",
            "repo_save_ms=",
        ] {
            assert!(logs.contains(field), "missing {field}; logs: {logs}");
        }
        assert!(
            !logs.contains("server_processing_duration_ms="),
            "edit-stage logs must not reuse the authoritative tool-boundary field; logs: {logs}"
        );
    }

    #[test]
    fn snippet_short_content_not_truncated() {
        let content = "Hello, world!";
        let (snippet, content_length, truncated) = build_snippet(content);
        assert_eq!(snippet, "Hello, world!");
        assert_eq!(content_length, 13);
        assert!(!truncated);
    }

    #[test]
    fn snippet_exact_limit_not_truncated() {
        let content: String = "a".repeat(SNIPPET_MAX_CHARS);
        let (snippet, content_length, truncated) = build_snippet(&content);
        assert_eq!(snippet, content);
        assert_eq!(content_length, SNIPPET_MAX_CHARS);
        assert!(!truncated);
    }

    #[test]
    fn snippet_over_limit_is_truncated() {
        let content: String = "b".repeat(SNIPPET_MAX_CHARS + 100);
        let (snippet, content_length, truncated) = build_snippet(&content);
        assert_eq!(snippet.chars().count(), SNIPPET_MAX_CHARS);
        assert_eq!(content_length, SNIPPET_MAX_CHARS + 100);
        assert!(truncated);
    }

    #[test]
    fn snippet_counts_unicode_chars_not_bytes() {
        // Each emoji is 1 char but multiple bytes.
        let emoji_content: String = "\u{1F600}".repeat(SNIPPET_MAX_CHARS + 1);
        let (snippet, content_length, truncated) = build_snippet(&emoji_content);
        assert_eq!(snippet.chars().count(), SNIPPET_MAX_CHARS);
        assert_eq!(content_length, SNIPPET_MAX_CHARS + 1);
        assert!(truncated);
    }

    #[test]
    fn snippet_empty_content() {
        let (snippet, content_length, truncated) = build_snippet("");
        assert_eq!(snippet, "");
        assert_eq!(content_length, 0);
        assert!(!truncated);
    }

    // -----------------------------------------------------------------------
    // Wire-contract tests — the recall result entry shape MCP clients see.
    // -----------------------------------------------------------------------

    fn wire_test_memory() -> Memory {
        let metadata = MemoryMetadata::new(Scope::Root, vec!["tag-a".to_string()], None);
        Memory::new("wire-test", "some content".to_string(), metadata).expect("valid memory")
    }

    fn entry_keys(entry: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("entry must be an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn recall_entry_has_exactly_the_contract_fields() {
        let hit = FusedHit {
            qualified_name: "v1:scope=global;name=wire-test".to_string(),
            semantic_distance: Some(0.25),
            lexical_score: Some(4.2),
            score: 0.03,
        };

        let entry = recall_entry_json(&wire_test_memory(), &hit);

        assert_eq!(
            entry_keys(&entry),
            vec![
                "content",
                "content_length",
                "distance",
                "id",
                "match_type",
                "name",
                "scope",
                "tags",
                "truncated",
            ]
        );
    }

    #[test]
    fn recall_entry_semantic_hit_has_numeric_distance() {
        let hit = FusedHit {
            qualified_name: "v1:scope=global;name=wire-test".to_string(),
            semantic_distance: Some(0.25),
            lexical_score: None,
            score: 0.02,
        };

        let entry = recall_entry_json(&wire_test_memory(), &hit);

        let distance = entry["distance"].as_f64().expect("distance is a number");
        assert!((distance - 0.25).abs() < 1e-6);
        assert_eq!(entry["match_type"], "semantic");
    }

    #[test]
    fn recall_entry_lexical_only_hit_has_numeric_sentinel_distance() {
        let hit = FusedHit {
            qualified_name: "v1:scope=global;name=wire-test".to_string(),
            semantic_distance: None,
            lexical_score: Some(7.5),
            score: 0.016,
        };

        let entry = recall_entry_json(&wire_test_memory(), &hit);

        // The pre-hybrid wire contract has `distance` as a required numeric
        // field; lexical-only hits must keep it numeric (never null/absent)
        // so strict older clients still deserialize the entry.
        let distance = entry["distance"]
            .as_f64()
            .expect("distance must stay numeric for lexical-only hits");
        assert!((distance - LEXICAL_ONLY_DISTANCE_SENTINEL).abs() < 1e-12);
        assert_eq!(entry["match_type"], "lexical");
    }

    #[test]
    fn recall_entry_lexical_only_hit_keeps_the_full_contract_shape() {
        // Both hit shapes — with a real distance and with the sentinel —
        // must expose exactly the same field set.
        let hit = FusedHit {
            qualified_name: "v1:scope=global;name=wire-test".to_string(),
            semantic_distance: None,
            lexical_score: Some(7.5),
            score: 0.016,
        };

        let entry = recall_entry_json(&wire_test_memory(), &hit);

        assert_eq!(
            entry_keys(&entry),
            vec![
                "content",
                "content_length",
                "distance",
                "id",
                "match_type",
                "name",
                "scope",
                "tags",
                "truncated",
            ]
        );
    }

    #[test]
    fn recall_entry_deserializes_for_a_strict_numeric_distance_client() {
        // Simulates a pre-hybrid client that models `distance` as a
        // required f32 — both hit shapes must satisfy it.
        #[derive(serde::Deserialize)]
        struct StrictClientEntry {
            #[allow(dead_code)]
            name: String,
            distance: f32,
        }

        for semantic_distance in [Some(0.25_f32), None] {
            let hit = FusedHit {
                qualified_name: "v1:scope=global;name=wire-test".to_string(),
                semantic_distance,
                lexical_score: Some(7.5),
                score: 0.016,
            };
            let entry = recall_entry_json(&wire_test_memory(), &hit);
            let parsed: StrictClientEntry = serde_json::from_value(entry)
                .expect("strict numeric-distance clients must keep working");
            assert!(
                parsed.distance >= -1.0,
                "sentinel is the only negative value"
            );
        }
    }

    #[test]
    fn recall_entry_both_hit_keeps_semantic_distance() {
        let hit = FusedHit {
            qualified_name: "v1:scope=global;name=wire-test".to_string(),
            semantic_distance: Some(0.31),
            lexical_score: Some(2.0),
            score: 0.033,
        };

        let entry = recall_entry_json(&wire_test_memory(), &hit);

        assert_eq!(entry["match_type"], "both");
        assert!(entry["distance"].is_number());
    }

    // -----------------------------------------------------------------------
    // Lexical failure/repair receipts (#310, ADR-0039)
    //
    // Failure injection across the mutation paths that mirror git writes
    // into the lexical index. Each receipt asserts the contract end to end:
    // the handler stays best-effort (the git write succeeds), the failed
    // mirror flags the index degraded, and the repair loop deterministically
    // converges the index back to git truth.
    // -----------------------------------------------------------------------

    mod lexical_failure_receipts {
        use super::*;
        use crate::auth::AuthProvider;
        use crate::health::HealthRegistry;
        use crate::index::InMemoryStore;
        use crate::repo::MemoryRepo;
        use crate::search::bm25::FailPoint;
        use crate::search::rebuild_lexical_from_repo;
        use async_trait::async_trait;

        struct MockEmbedding;

        #[async_trait]
        impl crate::embedding::EmbeddingBackend for MockEmbedding {
            async fn embed(
                &self,
                texts: &[String],
            ) -> Result<Vec<Vec<f32>>, crate::error::MemoryError> {
                Ok(texts.iter().map(|_| vec![0.0, 0.0, 0.0, 1.0]).collect())
            }

            fn dimensions(&self) -> usize {
                4
            }
        }

        fn parts() -> http::request::Parts {
            http::Request::builder()
                .uri("/")
                .body(())
                .expect("request")
                .into_parts()
                .0
        }

        fn test_state(tmp: &tempfile::TempDir) -> Arc<AppState> {
            let repo = MemoryRepo::init_or_open(tmp.path(), None).expect("repo init");
            Arc::new(AppState::new(
                Arc::new(repo),
                "main".to_string(),
                Box::new(MockEmbedding),
                Box::new(InMemoryStore::new(4)),
                AuthProvider::new(),
                HealthRegistry::new(),
                None,
            ))
        }

        fn qualified(scope: &Scope, name: &str) -> String {
            MemoryRef::new(scope.clone(), MemoryName::new(name.to_string()).unwrap())
                .qualified_path()
        }

        /// Poll until the lexical index is consistent and `query` returns
        /// exactly `expected` — the deterministic-convergence receipt. The
        /// repair task runs in the background, so convergence is eventual
        /// but bounded.
        async fn assert_lexical_converges(state: &Arc<AppState>, query: &str, expected: &[String]) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if !state.lexical.is_degraded() {
                    if let Ok(hits) = state.lexical.search(&ScopeFilter::All, query, 10) {
                        let names: Vec<String> = hits.into_iter().map(|(n, _)| n).collect();
                        if names == expected {
                            return;
                        }
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "lexical index never converged to git truth for query {query:?} \
                     (expected {expected:?}, degraded={})",
                    state.lexical.is_degraded()
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }

        async fn remember(
            server: &MemoryServer,
            name: &str,
            content: &str,
        ) -> Result<String, ErrorData> {
            server
                .remember(
                    Parameters(RememberArgs {
                        content: content.to_string(),
                        name: name.to_string(),
                        tags: vec![],
                        scope: None,
                        source: None,
                    }),
                    Extension(parts()),
                )
                .await
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn remember_mirror_failure_degrades_then_repairs_to_git_truth() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);
            let server = MemoryServer::new(Arc::clone(&state));

            state.lexical.fail_next(FailPoint::Commit);
            remember(&server, "note", "rememberword content")
                .await
                .expect("remember must stay best-effort despite the lexical failure");

            // Git truth holds the memory even though the mirror failed.
            state
                .repo
                .read_memory(&MemoryName::new("note".to_string()).unwrap(), &Scope::Root)
                .await
                .expect("git truth must hold the memory");

            assert_lexical_converges(&state, "rememberword", &[qualified(&Scope::Root, "note")])
                .await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn edit_mirror_failure_degrades_then_repairs_to_git_truth() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);
            let server = MemoryServer::new(Arc::clone(&state));

            remember(&server, "note", "oldword content")
                .await
                .expect("remember");

            state.lexical.fail_next(FailPoint::Commit);
            server
                .edit(
                    Parameters(EditArgs {
                        name: "note".to_string(),
                        content: Some("newword content".to_string()),
                        tags: None,
                        scope: None,
                    }),
                    Extension(parts()),
                )
                .await
                .expect("edit must stay best-effort despite the lexical failure");

            // Post-repair: new content searchable, old content gone.
            assert_lexical_converges(&state, "newword", &[qualified(&Scope::Root, "note")]).await;
            let stale = state
                .lexical
                .search(&ScopeFilter::All, "oldword", 10)
                .expect("post-repair search");
            assert!(stale.is_empty(), "stale content survived repair: {stale:?}");
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn forget_mirror_failure_degrades_then_repairs_to_git_truth() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);
            let server = MemoryServer::new(Arc::clone(&state));

            remember(&server, "doomed", "doomedword content")
                .await
                .expect("remember");

            state.lexical.fail_next(FailPoint::Commit);
            server
                .forget(
                    Parameters(ForgetArgs {
                        name: "doomed".to_string(),
                        scope: None,
                    }),
                    Extension(parts()),
                )
                .await
                .expect("forget must stay best-effort despite the lexical failure");

            // Post-repair: the deleted memory is not lexically reachable.
            assert_lexical_converges(&state, "doomedword", &[]).await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn move_mirror_failure_degrades_then_repairs_to_git_truth() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);
            let server = MemoryServer::new(Arc::clone(&state));

            remember(&server, "wanderer", "moveword content")
                .await
                .expect("remember");

            state.lexical.fail_next(FailPoint::Commit);
            server
                .move_memory(
                    Parameters(MoveArgs {
                        name: "wanderer".to_string(),
                        from_scope: None,
                        to_scope: "proj".to_string(),
                        new_name: None,
                    }),
                    Extension(parts()),
                )
                .await
                .expect("move must stay best-effort despite the lexical failure");

            // Post-repair: only the destination entry exists.
            let dest_scope = Scope::parse_or_default(Some("proj")).unwrap();
            assert_lexical_converges(&state, "moveword", &[qualified(&dest_scope, "wanderer")])
                .await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn incremental_reindex_mirror_failure_degrades_then_repairs() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);

            // A memory lands in the repo out-of-band (as after a pull).
            let memory = Memory::from_validated(
                MemoryName::new("pulled".to_string()).unwrap(),
                "pulledword content".to_string(),
                MemoryMetadata::new(Scope::Root, vec![], None),
            );
            state.repo.save_memory(&memory).await.expect("save");

            let changes = ChangedMemories {
                upserted: vec![qualified(&Scope::Root, "pulled")],
                removed: vec![],
            };

            state.lexical.fail_next(FailPoint::Commit);
            let stats = incremental_reindex(
                &state.repo,
                state.embedding.as_ref(),
                state.index.as_ref(),
                &state.lexical,
                &changes,
            )
            .await;
            assert_eq!(stats.errors, 0, "vector-side reindex must be unaffected");

            assert_lexical_converges(&state, "pulledword", &[qualified(&Scope::Root, "pulled")])
                .await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn startup_rebuild_failure_degrades_then_retry_converges() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);

            let memory = Memory::from_validated(
                MemoryName::new("note".to_string()).unwrap(),
                "bootword content".to_string(),
                MemoryMetadata::new(Scope::Root, vec![], None),
            );
            state.repo.save_memory(&memory).await.expect("save");

            // Startup rebuild fails mid-way: index must be degraded, not
            // silently half-built.
            state.lexical.fail_next(FailPoint::BeforeOp(0));
            assert!(rebuild_lexical_from_repo(&state.repo, &state.lexical)
                .await
                .is_err());
            assert!(state.lexical.is_degraded());
            assert!(
                state
                    .lexical
                    .search(&ScopeFilter::All, "bootword", 10)
                    .is_err(),
                "degraded index must not serve results"
            );

            // The retry (same code path as repair) converges.
            rebuild_lexical_from_repo(&state.repo, &state.lexical)
                .await
                .expect("retry rebuild");
            assert!(!state.lexical.is_degraded());
            let hits = state
                .lexical
                .search(&ScopeFilter::All, "bootword", 10)
                .expect("post-repair search");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].0, qualified(&Scope::Root, "note"));
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn recall_on_degraded_index_serves_semantic_only_and_triggers_repair() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let state = test_state(&tmp);
            let server = MemoryServer::new(Arc::clone(&state));

            remember(&server, "note", "healme content")
                .await
                .expect("remember");
            state
                .lexical
                .mark_rebuild_required("test: forced divergence");
            assert!(state.lexical.is_degraded());

            // Recall must succeed (semantic-only) while degraded…
            server
                .recall(
                    Parameters(RecallArgs {
                        query: "healme".to_string(),
                        scope: None,
                        limit: Some(5),
                    }),
                    Extension(parts()),
                )
                .await
                .expect("recall must serve semantic-only while degraded");

            // …and it kicks a background repair that converges.
            assert_lexical_converges(&state, "healme", &[qualified(&Scope::Root, "note")]).await;
        }
    }
}
