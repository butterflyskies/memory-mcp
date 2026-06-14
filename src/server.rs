use std::{sync::Arc, time::Instant};

/// Maximum number of characters included in recall result snippets.
/// Content longer than this is truncated and flagged with `truncated: true`.
const SNIPPET_MAX_CHARS: usize = 500;

use chrono::Utc;
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Extension, wrapper::Parameters},
    model::{ErrorData, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler,
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

// ---------------------------------------------------------------------------
// Incremental reindex helper
// ---------------------------------------------------------------------------

/// Re-embed and re-index all memories that changed between two commits.
///
/// Removals are processed first so a name that was deleted and re-added in
/// the same pull gets a fresh entry rather than a ghost.
async fn incremental_reindex(
    repo: &Arc<MemoryRepo>,
    embedding: &dyn EmbeddingBackend,
    index: &dyn VectorStore,
    changes: &ChangedMemories,
) -> ReindexStats {
    let mut stats = ReindexStats::default();

    // ---- 1. Removals --------------------------------------------------------
    for name in &changes.removed {
        match parse_qualified_name(name) {
            Ok(mref) => {
                let canonical = mref.qualified_path();
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
        to_embed.push((mref.clone(), memory.content));
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
                .add(&scope, &vector, qualified_name)
                .map_err(ErrorData::from)?;

            let start = Instant::now();
            state
                .repo
                .save_memory(&memory)
                .await
                .map_err(ErrorData::from)?;
            info!(repo_ms = start.elapsed().as_millis(), "saved to repo");

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
        description = "Search memories by semantic similarity. Embeds the query and returns the top matching memories as a JSON array \
        with name, scope, tags, and a content snippet (max 500 chars).\n\n\
        Each result includes `truncated` (bool) and `content_length` (total character count). \
        When `truncated` is true, the snippet is incomplete — use the `read` tool with the memory's name and scope \
        to retrieve the full content before acting on it.\n\n\
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

            let start = Instant::now();
            let query_vector = state
                .embedding
                .embed_one(&args.query)
                .await
                .map_err(ErrorData::from)?;
            info!(embed_ms = start.elapsed().as_millis(), "query embedded");

            let start = Instant::now();
            let results = state
                .index
                .search(&scope_filter, &query_vector, limit)
                .map_err(ErrorData::from)?;
            info!(
                search_ms = start.elapsed().as_millis(),
                candidates = results.len(),
                "index searched"
            );

            let pre_filter_count = results.len();
            let mut results_vec = Vec::new();
            let mut log_entries: Vec<RecallResult> = Vec::new();
            let mut skipped_errors: usize = 0;

            for (_key, qualified_name, distance) in results {
                // The index returns at most `limit` candidates; this guard is a safety
                // net that only activates if more candidates arrive than expected.
                if results_vec.len() >= limit {
                    break;
                }
                let mref = match parse_qualified_name(&qualified_name) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            qualified_name = %qualified_name,
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
                let (snippet, content_length, truncated) = build_snippet(&memory.content);

                log_entries.push(RecallResult {
                    memory_name: memory.name.to_string(),
                    scope: memory.metadata.scope.to_string(),
                    rank,
                    distance: distance as f64,
                });

                results_vec.push(serde_json::json!({
                    "id": memory.id,
                    "name": memory.name,
                    "scope": memory.metadata.scope.to_string(),
                    "tags": memory.metadata.tags,
                    "content": snippet,
                    "content_length": content_length,
                    "truncated": truncated,
                    "distance": distance,
                }));
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

            let start = Instant::now();

            // Track whether content changed so we can skip re-embedding when only tags changed.
            let content_changed = args.content.is_some();

            // Read the existing memory.
            let mut memory = state
                .repo
                .read_memory(&name, &scope)
                .await
                .map_err(ErrorData::from)?;

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
                let vector = state
                    .embedding
                    .embed_one(&memory.content)
                    .await
                    .map_err(ErrorData::from)?;

                state
                    .index
                    .add(&scope, &vector, qualified_name)
                    .map_err(ErrorData::from)?;
            }

            // Persist to repo (last, so partial failures leave recoverable state).
            state
                .repo
                .save_memory(&memory)
                .await
                .map_err(ErrorData::from)?;

            info!(
                ms = start.elapsed().as_millis(),
                name = %name,
                content_changed,
                "memory edited"
            );

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
                .add(&to_scope, &vector, dest_qualified)
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
}
