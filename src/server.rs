use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler,
};

use crate::types::{
    AppState, EditArgs, ForgetArgs, ListArgs, ReadArgs, RecallArgs, RememberArgs, SyncArgs,
};

/// MCP server implementation.
///
/// Each tool method is a stub that returns a "not yet implemented" string.
/// Tool methods return `String` because `String` implements `IntoContents`
/// and therefore `IntoCallToolResult`, which is what `#[tool_router]` requires.
/// The full business logic will be wired in once the transport layer is
/// validated end-to-end.
#[derive(Clone)]
pub struct MemoryServer {
    state: Arc<AppState>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MemoryServer {
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
    fn remember(&self, Parameters(args): Parameters<RememberArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn recall(&self, Parameters(args): Parameters<RecallArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn forget(&self, Parameters(args): Parameters<ForgetArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn edit(&self, Parameters(args): Parameters<EditArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn list(&self, Parameters(args): Parameters<ListArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn read(&self, Parameters(args): Parameters<ReadArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
    fn sync(&self, Parameters(args): Parameters<SyncArgs>) -> String {
        let _ = (&self.state, args);
        "not yet implemented".to_string()
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
