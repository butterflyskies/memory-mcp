use std::{sync::Arc, time::Instant};

use chrono::Utc;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ErrorData, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler,
};
use tracing::{info, warn, Instrument};

use crate::{
    embedding::EmbeddingBackend,
    error::MemoryError,
    index::VectorIndex,
    repo::MemoryRepo,
    types::{
        parse_qualified_name, parse_scope, validate_name, AppState, ChangedMemories, EditArgs,
        ForgetArgs, ListArgs, Memory, MemoryMetadata, PullResult, ReadArgs, RecallArgs,
        ReindexStats, RememberArgs, Scope, SyncArgs,
    },
};

/// MCP server implementation.
///
/// Each tool method is an async handler that calls into the backing subsystems
/// (git repo, embedding engine, HNSW index) and returns structured JSON.
#[derive(Clone)]
pub struct MemoryServer {
    state: Arc<AppState>,
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
    index: &VectorIndex,
    changes: &ChangedMemories,
) -> ReindexStats {
    let mut stats = ReindexStats::default();

    // ---- 1. Removals --------------------------------------------------------
    for name in &changes.removed {
        if let Some(key) = index.find_key_by_name(name) {
            if let Err(e) = index.remove(key) {
                warn!(
                    qualified_name = %name,
                    error = %e,
                    "incremental_reindex: failed to remove vector; skipping"
                );
                stats.errors += 1;
            } else {
                stats.removed += 1;
            }
        }
        // If not in index, nothing to do — not an error.
    }

    // ---- 2. Resolve (scope, name) pairs for upserts -------------------------
    // Each qualified name is "global/foo" or "projects/<project>/foo".
    // parse_qualified_name handles both forms.
    let mut pairs: Vec<(Scope, String, String)> = Vec::new(); // (scope, name, qualified_name)
    for qualified in &changes.upserted {
        match parse_qualified_name(qualified) {
            Ok((scope, name)) => pairs.push((scope, name, qualified.clone())),
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
    // (qualified_name, content, old_key_if_exists)
    let mut to_embed: Vec<(String, String, Option<u64>)> = Vec::new();
    for (scope, name, qualified) in &pairs {
        let old_key = index.find_key_by_name(qualified);
        let memory = match repo.read_memory(name, scope).await {
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
        to_embed.push((qualified.clone(), memory.content, old_key));
    }

    if to_embed.is_empty() {
        return stats;
    }

    // ---- 4. Batch embed all content -----------------------------------------
    let contents: Vec<String> = to_embed.iter().map(|(_, c, _)| c.clone()).collect();
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
                            qualified_name = %to_embed[i].0,
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
    for ((qualified_name, _, old_key), vector) in to_embed.iter().zip(vectors.iter()) {
        let is_update = old_key.is_some();

        // Add new entry first — if this fails, leave the old entry intact.
        match index.add_with_next_key(vector, qualified_name.clone()) {
            Ok(_) => {}
            Err(e) => {
                warn!(
                    qualified_name = %qualified_name,
                    error = %e,
                    "incremental_reindex: add_with_next_key failed; skipping"
                );
                stats.errors += 1;
                continue;
            }
        }

        // Now remove the old entry (new one already in the index).
        if let Some(key) = old_key {
            if let Err(e) = index.remove(*key) {
                warn!(
                    qualified_name = %qualified_name,
                    error = %e,
                    "incremental_reindex: old vector removal failed; continuing"
                );
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
        description = "Store a new memory. Saves the content to the git-backed repository \
        and indexes it for semantic search. Returns the assigned memory ID."
    )]
    async fn remember(
        &self,
        Parameters(args): Parameters<RememberArgs>,
    ) -> Result<String, ErrorData> {
        validate_name(&args.name).map_err(ErrorData::from)?;
        if args.content.len() > MAX_CONTENT_SIZE {
            return Err(ErrorData::from(crate::error::MemoryError::InvalidInput {
                reason: format!(
                    "content size {} exceeds maximum of {} bytes",
                    args.content.len(),
                    MAX_CONTENT_SIZE
                ),
            }));
        }
        let span = tracing::info_span!(
            "remember",
            name = %args.name,
            scope = ?args.scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = parse_scope(args.scope.as_deref()).map_err(ErrorData::from)?;
            let metadata = MemoryMetadata::new(scope, args.tags, args.source);
            let memory = Memory::new(args.name, args.content, metadata);

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

            let qualified_name = format!("{}/{}", memory.metadata.scope.dir_prefix(), memory.name);

            // Look up any existing key BEFORE adding — we remove it only after
            // the new entry is successfully inserted, so a failure in
            // add_with_next_key never leaves the memory orphaned.
            let old_key = state.index.find_key_by_name(&qualified_name);

            state
                .index
                .add_with_next_key(&vector, qualified_name)
                .map_err(ErrorData::from)?;

            // Now safe to remove the old entry (new one is already in the index).
            if let Some(old_key) = old_key {
                if let Err(e) = state.index.remove(old_key) {
                    warn!(
                        name = %memory.name,
                        error = %e,
                        "old vector removal failed during remember; continuing"
                    );
                }
            }

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
        description = "Search memories by semantic similarity. Embeds the query and returns \
        the top matching memories as a JSON array with name, scope, tags, and content snippet."
    )]
    async fn recall(&self, Parameters(args): Parameters<RecallArgs>) -> Result<String, ErrorData> {
        let span = tracing::info_span!(
            "recall",
            query = %args.query,
            scope = ?args.scope,
            limit = ?args.limit,
        );
        let state = Arc::clone(&self.state);
        async move {
            // Parse optional scope filter.
            let scope_filter: Option<Scope> = args
                .scope
                .as_deref()
                .map(|s| s.parse::<Scope>())
                .transpose()
                .map_err(ErrorData::from)?;

            let limit = args.limit.unwrap_or(5).min(100);
            // Over-fetch to compensate for scope filtering.
            let fetch_limit = limit.saturating_mul(3);

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
                .search(&query_vector, fetch_limit)
                .map_err(ErrorData::from)?;
            info!(
                search_ms = start.elapsed().as_millis(),
                candidates = results.len(),
                "index searched"
            );

            let pre_filter_count = results.len();
            let mut results_vec = Vec::new();
            let mut scope_filtered: usize = 0;
            let mut skipped_errors: usize = 0;

            for (_key, qualified_name, distance) in results {
                if results_vec.len() >= limit {
                    break;
                }
                let (scope, name) = match parse_qualified_name(&qualified_name) {
                    Ok(pair) => pair,
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

                // Apply scope filter if present.
                if let Some(ref filter) = scope_filter {
                    if &scope != filter {
                        scope_filtered += 1;
                        continue;
                    }
                }

                // Read the memory; if it was deleted but still in the index, skip it.
                let memory = match state.repo.read_memory(&name, &scope).await {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(
                            name = %name,
                            error = %e,
                            "could not read memory from repo (deleted?); skipping"
                        );
                        skipped_errors += 1;
                        continue;
                    }
                };

                // Truncate content to 500 chars for the snippet.
                let snippet: String = memory.content.chars().take(500).collect();

                results_vec.push(serde_json::json!({
                    "id": memory.id,
                    "name": memory.name,
                    "scope": memory.metadata.scope.to_string(),
                    "tags": memory.metadata.tags,
                    "content": snippet,
                    "distance": distance,
                }));
            }

            info!(
                returned = results_vec.len(),
                scope_filtered, skipped_errors, "recall complete"
            );

            Ok(serde_json::json!({
                "results": results_vec,
                "count": results_vec.len(),
                "limit": limit,
                "pre_filter_count": pre_filter_count,
                "scope_filtered": scope_filtered,
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
        description = "Delete a memory by name. Removes the file from git and the vector from \
        the search index. Returns 'ok' on success."
    )]
    async fn forget(&self, Parameters(args): Parameters<ForgetArgs>) -> Result<String, ErrorData> {
        validate_name(&args.name).map_err(ErrorData::from)?;
        let span = tracing::info_span!(
            "forget",
            name = %args.name,
            scope = ?args.scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = parse_scope(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();

            let qualified_name = format!("{}/{}", scope.dir_prefix(), args.name);

            // Look up the vector key by name instead of deriving it from UUID.
            match state.index.find_key_by_name(&qualified_name) {
                Some(key) => {
                    if let Err(e) = state.index.remove(key) {
                        warn!(
                            name = %args.name,
                            error = %e,
                            "vector removal failed during forget; continuing"
                        );
                    }
                }
                None => {
                    warn!(
                        name = %args.name,
                        "vector key not found in index during forget; continuing"
                    );
                }
            }

            state
                .repo
                .delete_memory(&args.name, &scope)
                .await
                .map_err(ErrorData::from)?;

            info!(
                ms = start.elapsed().as_millis(),
                name = %args.name,
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
        tags to preserve existing values. Re-embeds and re-indexes the memory. Returns the memory ID."
    )]
    async fn edit(&self, Parameters(args): Parameters<EditArgs>) -> Result<String, ErrorData> {
        validate_name(&args.name).map_err(ErrorData::from)?;
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
        let span = tracing::info_span!(
            "edit",
            name = %args.name,
            scope = ?args.scope,
        );
        let state = Arc::clone(&self.state);
        async move {
            let scope = parse_scope(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();

            // Track whether content changed so we can skip re-embedding when only tags changed.
            let content_changed = args.content.is_some();

            // Read the existing memory.
            let mut memory = state
                .repo
                .read_memory(&args.name, &scope)
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
            // Order: (1) embed, (2) remove old + add new index entry, (3) save to repo.
            if content_changed {
                let qualified_name =
                    format!("{}/{}", memory.metadata.scope.dir_prefix(), memory.name);

                // Re-embed updated content.
                let vector = state
                    .embedding
                    .embed_one(&memory.content)
                    .await
                    .map_err(ErrorData::from)?;

                // Add new entry first, then remove old — so a failure in
                // add_with_next_key never leaves the memory orphaned.
                let old_key = state.index.find_key_by_name(&qualified_name);

                state
                    .index
                    .add_with_next_key(&vector, qualified_name)
                    .map_err(ErrorData::from)?;

                if let Some(old_key) = old_key {
                    if let Err(e) = state.index.remove(old_key) {
                        warn!(
                            name = %args.name,
                            error = %e,
                            "old vector key removal failed during edit; continuing"
                        );
                    }
                }
            }

            // Persist to repo (last, so partial failures leave recoverable state).
            state
                .repo
                .save_memory(&memory)
                .await
                .map_err(ErrorData::from)?;

            info!(
                ms = start.elapsed().as_millis(),
                name = %args.name,
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

    /// List stored memories, optionally filtered by scope.
    ///
    /// Returns a JSON array of memory summaries (id, name, scope, tags,
    /// created_at, updated_at). Full content bodies are omitted for brevity.
    #[tool(
        name = "list",
        description = "List stored memories. Optionally filter by scope ('global' or \
        'project:<name>'). Returns a JSON array of memory summaries without full content."
    )]
    async fn list(&self, Parameters(args): Parameters<ListArgs>) -> Result<String, ErrorData> {
        let span = tracing::info_span!("list", scope = ?args.scope);
        let state = Arc::clone(&self.state);
        async move {
            // None means all scopes.
            let scope_filter: Option<Scope> = args
                .scope
                .as_deref()
                .map(|s| s.parse::<Scope>())
                .transpose()
                .map_err(ErrorData::from)?;

            let start = Instant::now();
            let memories = state
                .repo
                .list_memories(scope_filter.as_ref())
                .await
                .map_err(ErrorData::from)?;
            info!(
                ms = start.elapsed().as_millis(),
                count = memories.len(),
                "listed memories"
            );

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
                "count": summaries.len(),
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
        description = "Read a specific memory by name. Returns the full markdown content and \
        metadata (id, scope, tags, timestamps) as a JSON object."
    )]
    async fn read(&self, Parameters(args): Parameters<ReadArgs>) -> Result<String, ErrorData> {
        validate_name(&args.name).map_err(ErrorData::from)?;
        let span = tracing::info_span!("read", name = %args.name, scope = ?args.scope);
        let state = Arc::clone(&self.state);
        async move {
            let scope = parse_scope(args.scope.as_deref()).map_err(ErrorData::from)?;

            let start = Instant::now();
            let memory = state
                .repo
                .read_memory(&args.name, &scope)
                .await
                .map_err(ErrorData::from)?;
            info!(
                ms = start.elapsed().as_millis(),
                name = %args.name,
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
    async fn sync(&self, Parameters(args): Parameters<SyncArgs>) -> Result<String, ErrorData> {
        let pull_first = args.pull_first.unwrap_or(true);
        let span = tracing::info_span!("sync", pull_first);
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
                    let changes = tokio::task::spawn_blocking(move || {
                        repo.diff_changed_memories(old_head, new_head)
                    })
                    .await
                    .map_err(|e| MemoryError::Join(e.to_string()))
                    .map_err(ErrorData::from)?
                    .map_err(ErrorData::from)?;

                    if !changes.is_empty() {
                        let stats = incremental_reindex(
                            &state.repo,
                            state.embedding.as_ref(),
                            &state.index,
                            &changes,
                        )
                        .await;
                        info!(
                            added = stats.added,
                            updated = stats.updated,
                            removed = stats.removed,
                            errors = stats.errors,
                            "incremental reindex complete"
                        );
                        reindex_stats = Some(stats);
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
}

#[tool_handler]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "A semantic memory system for AI coding agents. Memories are stored as \
                 markdown files in a git repository and indexed for semantic retrieval. \
                 Use `remember` to store, `recall` to search, `read` to fetch a specific \
                 memory, `edit` to update, `forget` to delete, `list` to browse, and \
                 `sync` to push/pull the remote."
                .to_string(),
        )
    }
}
