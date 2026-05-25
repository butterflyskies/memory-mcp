use std::{fmt, sync::Arc};

use rmcp::schemars;
use serde::Deserialize;

use crate::{
    auth::AuthProvider, embedding::EmbeddingBackend, health::HealthRegistry, index::VectorStore,
    repo::MemoryRepo,
};

// ---------------------------------------------------------------------------
// Tool argument structs
// ---------------------------------------------------------------------------

/// Arguments for the `remember` tool — store a new memory.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    /// The content to store. Markdown is supported.
    pub content: String,
    /// Human-readable name for this memory (used as the filename stem).
    pub name: String,
    /// Optional list of tags for categorisation.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Scope: 'global' or a bare namespace path like 'my-project' or 'org/team'. Defaults to 'global'.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional hint about the source of this memory.
    #[serde(default)]
    pub source: Option<String>,
}

/// Arguments for the `recall` tool — semantic search.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallArgs {
    /// Natural-language query to search for.
    pub query: String,
    /// Scope: 'global', a bare namespace path like 'my-project' or 'org/team', 'all', or omit for global-only. Use the basename of your cwd (or its path) to search your current namespace + global memories. Use 'all' to search across every scope.
    #[serde(default)]
    pub scope: Option<String>,
    /// Maximum number of results to return. Defaults to 5.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Arguments for the `forget` tool — delete a memory.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ForgetArgs {
    /// Exact name of the memory to delete.
    pub name: String,
    /// Scope of the memory. Defaults to 'global'. Use a bare namespace path like 'my-project' or 'org/team' for scoped memories.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `edit` tool — modify an existing memory.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    /// Name of the memory to edit.
    pub name: String,
    /// New content (replaces existing). Omit to keep current content.
    #[serde(default)]
    pub content: Option<String>,
    /// New tag list (replaces existing). Omit to keep current tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Scope of the memory. Defaults to 'global'. Use a bare namespace path like 'my-project' or 'org/team' for scoped memories.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `list` tool — browse stored memories.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    /// Scope: 'global', a bare namespace path like 'my-project' or 'org/team', 'all', or omit for global-only. Use 'all' to list everything.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `read` tool — retrieve a specific memory by name.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// Exact name of the memory to read.
    pub name: String,
    /// Scope of the memory. Defaults to 'global'. Use a bare namespace path like 'my-project' or 'org/team' for scoped memories.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Agent's assessment of whether a recalled memory was useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Memory materially influenced the session.
    Applied,
    /// Memory was partially relevant or influence was uncertain.
    Maybe,
    /// Memory was not relevant to the session.
    NotApplied,
}

impl Verdict {
    /// String representation for SQLite storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Applied => "applied",
            Verdict::Maybe => "maybe",
            Verdict::NotApplied => "not_applied",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Arguments for the `mark_applied` tool — report memory usage back to the recall log.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MarkAppliedArgs {
    /// The recall_id from the recall response that returned this memory.
    pub recall_id: String,
    /// Name of the memory that was (or was not) applied.
    pub memory: String,
    /// Agent's assessment of whether the memory was useful: 'applied', 'maybe', or 'not_applied'.
    pub verdict: Verdict,
    /// Brief description of how the memory influenced the session.
    #[serde(default)]
    pub application: Option<String>,
    /// Confidence level: "high", "medium", or "low".
    #[serde(default = "default_confidence")]
    pub confidence: String,
}

fn default_confidence() -> String {
    "medium".to_string()
}

/// Arguments for the `sync` tool — push/pull the git remote.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncArgs {
    /// If true, pull before pushing. Defaults to true.
    #[serde(default)]
    pub pull_first: Option<bool>,
}

// ---------------------------------------------------------------------------
// PullResult
/// Arguments for the `recall_stats` tool — no parameters required.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallStatsArgs {}

// ---------------------------------------------------------------------------

/// The outcome of a `pull()` operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum PullResult {
    /// No `origin` remote is configured — running in local-only mode.
    NoRemote,
    /// The local branch was already up to date with the remote.
    UpToDate,
    /// The remote was ahead and the branch was fast-forwarded.
    FastForward {
        /// Commit OID before the fast-forward.
        old_head: [u8; 20],
        /// Commit OID after the fast-forward.
        new_head: [u8; 20],
    },
    /// A merge was performed; `conflicts_resolved` counts auto-resolved files.
    Merged {
        /// Number of conflicting files that were auto-resolved.
        conflicts_resolved: usize,
        /// Commit OID before the merge.
        old_head: [u8; 20],
        /// Commit OID after the merge.
        new_head: [u8; 20],
    },
}

// ---------------------------------------------------------------------------
// ChangedMemories
// ---------------------------------------------------------------------------

/// Memories that changed between two git commits.
#[derive(Debug, Default)]
pub struct ChangedMemories {
    /// Qualified names (e.g. `"global/foo"`) that were added or modified.
    pub upserted: Vec<String>,
    /// Qualified names that were deleted.
    pub removed: Vec<String>,
}

impl ChangedMemories {
    /// Returns `true` if there are no changes.
    pub fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty()
    }
}

// ---------------------------------------------------------------------------
// ReindexStats
// ---------------------------------------------------------------------------

/// Statistics from an incremental reindex operation.
#[derive(Debug, Default)]
pub struct ReindexStats {
    /// Number of newly indexed memories.
    pub added: usize,
    /// Number of memories whose embeddings were refreshed.
    pub updated: usize,
    /// Number of memories removed from the index.
    pub removed: usize,
    /// Number of memories that failed to index.
    pub errors: usize,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Shared application state threaded through the Axum server.
///
/// Wrapped in a single outer `Arc` at the call site. `repo` is additionally
/// wrapped in its own `Arc` so it can be cloned into `spawn_blocking` closures.
#[non_exhaustive]
pub struct AppState {
    /// Git-backed memory repository.
    pub repo: Arc<MemoryRepo>,
    /// Backend used to compute text embeddings.
    pub embedding: Box<dyn EmbeddingBackend>,
    /// In-memory vector index for semantic search (scope-partitioned).
    pub index: Box<dyn VectorStore>,
    /// Authentication provider for API access control.
    pub auth: AuthProvider,
    /// Branch name used for push/pull operations (default: "main").
    pub branch: String,
    /// Passive health registry — subsystems report here, `/readyz` reads here.
    pub health: HealthRegistry,
    /// Optional append-only recall event log for threshold calibration.
    pub recall_log: Option<Arc<crate::recall_log::RecallLog>>,
}

impl AppState {
    /// Create a new application state from subsystem instances.
    pub fn new(
        repo: Arc<MemoryRepo>,
        branch: String,
        embedding: Box<dyn EmbeddingBackend>,
        index: Box<dyn VectorStore>,
        auth: AuthProvider,
        health: HealthRegistry,
        recall_log: Option<Arc<crate::recall_log::RecallLog>>,
    ) -> Self {
        Self {
            repo,
            embedding,
            index,
            auth,
            branch,
            health,
            recall_log,
        }
    }
}
