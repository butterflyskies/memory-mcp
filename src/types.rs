use chrono::{DateTime, Utc};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

use crate::error::MemoryError;

// ---------------------------------------------------------------------------
// Name validation
// ---------------------------------------------------------------------------

/// Validate that a memory name or project name contains only safe characters.
///
/// Allowed: alphanumeric, hyphens, underscores, dots, and forward slashes
/// (for nested paths). Dots may not start a component (no `..`). The name
/// must not be empty.
pub fn validate_name(name: &str) -> Result<(), MemoryError> {
    if name.is_empty() {
        return Err(MemoryError::InvalidInput {
            reason: "name must not be empty".to_string(),
        });
    }

    let components: Vec<&str> = name.split('/').collect();

    if components.len() > 3 {
        return Err(MemoryError::InvalidInput {
            reason: format!("name '{}' exceeds maximum nesting depth of 3", name),
        });
    }

    for component in &components {
        if component.is_empty() {
            return Err(MemoryError::InvalidInput {
                reason: format!("name '{}' contains an empty path component", name),
            });
        }
        if component.starts_with('.') {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "name '{}' contains a dot-prefixed component '{}'",
                    name, component
                ),
            });
        }
        if !component
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "name '{}' contains disallowed characters in component '{}'",
                    name, component
                ),
            });
        }
    }

    Ok(())
}

/// Validate a git branch name to prevent ref injection.
///
/// Rejects names that are empty, contain `..`, start or end with `/` or `.`,
/// contain consecutive slashes, or include characters that git disallows.
pub fn validate_branch_name(branch: &str) -> Result<(), MemoryError> {
    if branch.is_empty() {
        return Err(MemoryError::InvalidInput {
            reason: "branch name cannot be empty".into(),
        });
    }
    if branch.contains("..") {
        return Err(MemoryError::InvalidInput {
            reason: "branch name cannot contain '..'".into(),
        });
    }
    let invalid_chars = [' ', '~', '^', ':', '?', '*', '[', '\\'];
    for c in branch.chars() {
        if c.is_ascii_control() || invalid_chars.contains(&c) {
            return Err(MemoryError::InvalidInput {
                reason: format!("branch name contains invalid character '{}'", c),
            });
        }
    }
    if branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.starts_with('.')
    {
        return Err(MemoryError::InvalidInput {
            reason: "branch name has invalid start/end character".into(),
        });
    }
    if branch.contains("//") {
        return Err(MemoryError::InvalidInput {
            reason: "branch name contains consecutive slashes".into(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Where a memory lives on disk and conceptually.
///
/// - `Global`           → `global/`
/// - `Project(name)`    → `projects/{name}/`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "name")]
#[non_exhaustive]
pub enum Scope {
    /// Machine-wide memories, stored under `global/`.
    Global,
    /// Project-scoped memories, stored under `projects/{name}/`.
    Project(String),
}

impl Scope {
    /// Directory prefix inside the repo root.
    pub fn dir_prefix(&self) -> String {
        match self {
            Scope::Global => "global".to_string(),
            Scope::Project(name) => format!("projects/{}", name),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Global => write!(f, "global"),
            Scope::Project(name) => write!(f, "project:{}", name),
        }
    }
}

impl FromStr for Scope {
    type Err = MemoryError;

    /// Parse a scope string:
    /// - `"global"` → `Scope::Global`
    /// - `"project:{name}"` → `Scope::Project(name)`
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "global" {
            return Ok(Scope::Global);
        }
        if let Some(name) = s.strip_prefix("project:") {
            if name.is_empty() {
                return Err(MemoryError::InvalidInput {
                    reason: "project scope requires a non-empty name after 'project:'".to_string(),
                });
            }
            if name.contains('/') {
                return Err(MemoryError::InvalidInput {
                    reason: "project name must not contain '/'".to_string(),
                });
            }
            validate_name(name)?;
            return Ok(Scope::Project(name.to_string()));
        }
        Err(MemoryError::InvalidInput {
            reason: format!(
                "unrecognised scope '{}'; expected 'global' or 'project:<name>'",
                s
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// MemoryMetadata
// ---------------------------------------------------------------------------

/// Metadata attached to every [`Memory`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    /// Free-form tags for categorisation and filtering.
    pub tags: Vec<String>,
    /// Where this memory lives (global or project-scoped).
    pub scope: Scope,
    /// When this memory was first created.
    pub created_at: DateTime<Utc>,
    /// When this memory was last modified.
    pub updated_at: DateTime<Utc>,
    /// Optional hint about where this memory came from (e.g. a tool name).
    pub source: Option<String>,
}

impl MemoryMetadata {
    /// Create new metadata with the current timestamp for both `created_at` and `updated_at`.
    pub fn new(scope: Scope, tags: Vec<String>, source: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            tags,
            scope,
            created_at: now,
            updated_at: now,
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// A single memory unit, stored on disk as a markdown file with YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Stable UUID for vector-index keying.
    pub id: String,
    /// Human-readable name / filename stem.
    pub name: String,
    /// Markdown body (no frontmatter).
    pub content: String,
    /// Associated metadata (tags, scope, timestamps, source).
    pub metadata: MemoryMetadata,
}

impl Memory {
    /// Create a new memory with a random UUID.
    pub fn new(name: String, content: String, metadata: MemoryMetadata) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            content,
            metadata,
        }
    }

    /// Render to the on-disk format: YAML frontmatter + markdown body.
    ///
    /// Format:
    /// ```text
    /// ---
    /// <yaml>
    /// ---
    ///
    /// <content>
    /// ```
    pub fn to_markdown(&self) -> Result<String, MemoryError> {
        #[derive(Serialize)]
        struct Frontmatter<'a> {
            id: &'a str,
            name: &'a str,
            tags: &'a [String],
            scope: &'a Scope,
            created_at: &'a DateTime<Utc>,
            updated_at: &'a DateTime<Utc>,
            #[serde(skip_serializing_if = "Option::is_none")]
            source: Option<&'a str>,
        }

        let fm = Frontmatter {
            id: &self.id,
            name: &self.name,
            tags: &self.metadata.tags,
            scope: &self.metadata.scope,
            created_at: &self.metadata.created_at,
            updated_at: &self.metadata.updated_at,
            source: self.metadata.source.as_deref(),
        };

        let yaml = serde_yaml::to_string(&fm)?;
        Ok(format!("---\n{}---\n\n{}", yaml, self.content))
    }

    /// Parse from on-disk markdown format.
    pub fn from_markdown(raw: &str) -> Result<Self, MemoryError> {
        // Must start with "---\n"
        let rest = raw
            .strip_prefix("---\n")
            .ok_or_else(|| MemoryError::InvalidInput {
                reason: "missing opening frontmatter delimiter".to_string(),
            })?;

        // Find the closing "---"
        let end_marker = rest
            .find("\n---\n")
            .ok_or_else(|| MemoryError::InvalidInput {
                reason: "missing closing frontmatter delimiter".to_string(),
            })?;

        let yaml_str = &rest[..end_marker];
        // +5 = "\n---\n".len(); skip optional leading newline in body
        let body = rest[end_marker + 5..].trim_start_matches('\n');

        #[derive(Deserialize)]
        struct Frontmatter {
            id: String,
            name: String,
            tags: Vec<String>,
            scope: Scope,
            created_at: DateTime<Utc>,
            updated_at: DateTime<Utc>,
            source: Option<String>,
        }

        let fm: Frontmatter = serde_yaml::from_str(yaml_str)?;

        Ok(Memory {
            id: fm.id,
            name: fm.name,
            content: body.to_string(),
            metadata: MemoryMetadata {
                tags: fm.tags,
                scope: fm.scope,
                created_at: fm.created_at,
                updated_at: fm.updated_at,
                source: fm.source,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// ScopeFilter — for read-only queries (recall, list)
// ---------------------------------------------------------------------------

/// Controls which scopes are searched during read-only operations.
///
/// This is distinct from [`Scope`], which is a storage target for write
/// operations. `ScopeFilter` describes which memories are *returned*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeFilter {
    /// Search only global memories.
    GlobalOnly,
    /// Search a specific project's memories **and** global memories.
    ProjectAndGlobal(String),
    /// Search all scopes.
    All,
}

impl ScopeFilter {
    /// Returns `true` if `scope` passes this filter.
    ///
    /// - [`GlobalOnly`](ScopeFilter::GlobalOnly) → true only if scope is `Global`
    /// - [`ProjectAndGlobal(name)`](ScopeFilter::ProjectAndGlobal) → true if scope is `Global` or `Project(name)`
    /// - [`All`](ScopeFilter::All) → always true
    pub fn matches(&self, scope: &Scope) -> bool {
        match self {
            ScopeFilter::GlobalOnly => *scope == Scope::Global,
            ScopeFilter::ProjectAndGlobal(name) => match scope {
                Scope::Global => true,
                Scope::Project(p) => p == name,
            },
            ScopeFilter::All => true,
        }
    }
}

/// Parse a scope string into a [`ScopeFilter`] for use in `recall` and `list`.
///
/// | Input | Result |
/// |---|---|
/// | `None` | `GlobalOnly` |
/// | `"global"` | `GlobalOnly` |
/// | `"project:{name}"` | `ProjectAndGlobal(<name>)` |
/// | `"all"` | `All` |
pub fn parse_scope_filter(scope: Option<&str>) -> Result<ScopeFilter, MemoryError> {
    match scope {
        None | Some("global") => Ok(ScopeFilter::GlobalOnly),
        Some("all") => Ok(ScopeFilter::All),
        Some(s) => {
            let parsed = s.parse::<Scope>()?;
            match parsed {
                Scope::Project(name) => Ok(ScopeFilter::ProjectAndGlobal(name)),
                // "global" is already handled above; exhaustive match ensures
                // a compile error if new Scope variants are added.
                Scope::Global => Ok(ScopeFilter::GlobalOnly),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Parse an optional scope string. `None` defaults to `Scope::Global`.
pub fn parse_scope(scope: Option<&str>) -> Result<Scope, MemoryError> {
    match scope {
        None => Ok(Scope::Global),
        Some(s) => s.parse::<Scope>(),
    }
}

/// Parse a qualified name of the form `"global/<name>"` or
/// `"projects/<project>/<name>"` back into a `(Scope, name)` pair.
pub fn parse_qualified_name(qualified: &str) -> Result<(Scope, String), MemoryError> {
    if let Some(rest) = qualified.strip_prefix("global/") {
        validate_name(rest)?;
        return Ok((Scope::Global, rest.to_string()));
    }
    if let Some(rest) = qualified.strip_prefix("projects/") {
        // rest = "<project>/<memory_name>" (possibly nested)
        if let Some(slash_pos) = rest.find('/') {
            let project = &rest[..slash_pos];
            let name = &rest[slash_pos + 1..];
            if project.is_empty() || name.is_empty() {
                return Err(MemoryError::InvalidInput {
                    reason: format!(
                        "malformed qualified name '{}': project or memory name is empty",
                        qualified
                    ),
                });
            }
            validate_name(project)?;
            validate_name(name)?;
            return Ok((Scope::Project(project.to_string()), name.to_string()));
        }
        return Err(MemoryError::InvalidInput {
            reason: format!(
                "malformed qualified name '{}': missing memory name after project",
                qualified
            ),
        });
    }
    Err(MemoryError::InvalidInput {
        reason: format!(
            "malformed qualified name '{}': must start with 'global/' or 'projects/'",
            qualified
        ),
    })
}

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
    /// Scope: 'global' or 'project:{name}'. Defaults to 'global'. Use 'project:{basename-of-your-cwd}' for project-scoped storage.
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
    /// Scope: 'global', 'project:{name}', 'all', or omit for global-only. Use 'project:{basename-of-your-cwd}' to search your current project + global memories. Use 'all' to search across every scope.
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
    /// Scope of the memory. Defaults to 'global'. Use 'project:{basename-of-your-cwd}' for project-scoped memories.
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
    /// Scope of the memory. Defaults to 'global'. Use 'project:{basename-of-your-cwd}' for project-scoped memories.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `list` tool — browse stored memories.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    /// Scope: 'global', 'project:{name}', 'all', or omit for global-only. Use 'project:{basename-of-your-cwd}' to list project + global memories. Use 'all' to list everything.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `read` tool — retrieve a specific memory by name.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// Exact name of the memory to read.
    pub name: String,
    /// Scope of the memory. Defaults to 'global'. Use 'project:{basename-of-your-cwd}' for project-scoped memories.
    #[serde(default)]
    pub scope: Option<String>,
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

use std::sync::Arc;

use crate::{
    auth::AuthProvider, embedding::EmbeddingBackend, index::ScopedIndex, repo::MemoryRepo,
};

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
    pub index: ScopedIndex,
    /// Authentication provider for API access control.
    pub auth: AuthProvider,
    /// Branch name used for push/pull operations (default: "main").
    pub branch: String,
}

impl AppState {
    /// Create a new application state from subsystem instances.
    pub fn new(
        repo: Arc<MemoryRepo>,
        branch: String,
        embedding: Box<dyn EmbeddingBackend>,
        index: ScopedIndex,
        auth: AuthProvider,
    ) -> Self {
        Self {
            repo,
            embedding,
            index,
            auth,
            branch,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_memory() -> Memory {
        let meta = MemoryMetadata {
            tags: vec!["test".to_string(), "round-trip".to_string()],
            scope: Scope::Project("my-project".to_string()),
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: DateTime::from_timestamp(1_700_000_100, 0).unwrap(),
            source: Some("unit-test".to_string()),
        };
        Memory {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            name: "test-memory".to_string(),
            content: "# Hello\n\nThis is a test memory.".to_string(),
            metadata: meta,
        }
    }

    #[test]
    fn round_trip_markdown() {
        let original = make_memory();
        let rendered = original.to_markdown().expect("to_markdown should not fail");
        let parsed = Memory::from_markdown(&rendered).expect("from_markdown should not fail");

        assert_eq!(original.id, parsed.id);
        assert_eq!(original.name, parsed.name);
        assert_eq!(original.content, parsed.content);
        assert_eq!(original.metadata.tags, parsed.metadata.tags);
        assert_eq!(original.metadata.scope, parsed.metadata.scope);
        assert_eq!(
            original.metadata.created_at.timestamp(),
            parsed.metadata.created_at.timestamp()
        );
        assert_eq!(
            original.metadata.updated_at.timestamp(),
            parsed.metadata.updated_at.timestamp()
        );
        assert_eq!(original.metadata.source, parsed.metadata.source);
    }

    #[test]
    fn round_trip_global_scope() {
        let meta = MemoryMetadata::new(Scope::Global, vec!["global-tag".to_string()], None);
        let mem = Memory::new("global-mem".to_string(), "Some content.".to_string(), meta);
        let rendered = mem.to_markdown().unwrap();
        let parsed = Memory::from_markdown(&rendered).unwrap();

        assert_eq!(parsed.metadata.scope, Scope::Global);
        assert_eq!(parsed.metadata.source, None);
        assert_eq!(parsed.content, "Some content.");
    }

    #[test]
    fn round_trip_no_source() {
        let meta = MemoryMetadata::new(Scope::Project("proj".to_string()), vec![], None);
        let mem = Memory::new("no-src".to_string(), "Body.".to_string(), meta);
        let md = mem.to_markdown().unwrap();
        // source field should not appear in yaml
        assert!(!md.contains("source:"));
        let parsed = Memory::from_markdown(&md).unwrap();
        assert_eq!(parsed.metadata.source, None);
    }

    #[test]
    fn from_markdown_missing_frontmatter_fails() {
        let result = Memory::from_markdown("just plain text");
        assert!(result.is_err());
    }

    #[test]
    fn scope_dir_prefix() {
        assert_eq!(Scope::Global.dir_prefix(), "global");
        assert_eq!(
            Scope::Project("foo".to_string()).dir_prefix(),
            "projects/foo"
        );
    }

    #[test]
    fn scope_from_str_global() {
        assert_eq!("global".parse::<Scope>().unwrap(), Scope::Global);
    }

    #[test]
    fn scope_from_str_project() {
        assert_eq!(
            "project:my-proj".parse::<Scope>().unwrap(),
            Scope::Project("my-proj".to_string())
        );
    }

    #[test]
    fn scope_from_str_empty_project_name_fails() {
        assert!("project:".parse::<Scope>().is_err());
    }

    #[test]
    fn scope_from_str_unknown_fails() {
        assert!("unknown".parse::<Scope>().is_err());
        assert!("PROJECT:foo".parse::<Scope>().is_err());
    }

    #[test]
    fn scope_from_str_project_traversal_fails() {
        assert!("project:../../etc".parse::<Scope>().is_err());
    }

    // validate_name tests (moved from repo.rs)

    #[test]
    fn validate_name_accepts_valid() {
        assert!(validate_name("my-memory").is_ok());
        assert!(validate_name("some_memory").is_ok());
        assert!(validate_name("nested/path").is_ok());
        assert!(validate_name("v1.2.3").is_ok());
    }

    #[test]
    fn validate_name_rejects_traversal() {
        assert!(validate_name("../../etc/passwd").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("a/../b").is_err());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_special_chars() {
        assert!(validate_name("foo;bar").is_err());
        assert!(validate_name("foo bar").is_err());
        assert!(validate_name("foo\0bar").is_err());
    }

    #[test]
    fn validate_name_rejects_empty_component() {
        assert!(validate_name("foo//bar").is_err());
        assert!(validate_name("/absolute").is_err());
    }

    // parse_scope tests

    #[test]
    fn test_parse_scope_none_defaults_global() {
        assert_eq!(parse_scope(None).unwrap(), Scope::Global);
    }

    #[test]
    fn test_parse_scope_some_global() {
        assert_eq!(parse_scope(Some("global")).unwrap(), Scope::Global);
    }

    #[test]
    fn test_parse_scope_some_project() {
        assert_eq!(
            parse_scope(Some("project:my-proj")).unwrap(),
            Scope::Project("my-proj".to_string())
        );
    }

    // parse_qualified_name tests

    #[test]
    fn test_parse_qualified_name_global() {
        let (scope, name) = parse_qualified_name("global/my-memory").unwrap();
        assert_eq!(scope, Scope::Global);
        assert_eq!(name, "my-memory");
    }

    #[test]
    fn test_parse_qualified_name_project() {
        let (scope, name) = parse_qualified_name("projects/my-project/my-memory").unwrap();
        assert_eq!(scope, Scope::Project("my-project".to_string()));
        assert_eq!(name, "my-memory");
    }

    #[test]
    fn test_parse_qualified_name_nested() {
        let (scope, name) = parse_qualified_name("projects/my-project/nested/memory").unwrap();
        assert_eq!(scope, Scope::Project("my-project".to_string()));
        assert_eq!(name, "nested/memory");
    }

    // validate_branch_name tests

    #[test]
    fn validate_branch_name_accepts_valid() {
        assert!(validate_branch_name("main").is_ok());
        assert!(validate_branch_name("feature/foo").is_ok());
        assert!(validate_branch_name("release-1.0").is_ok());
        assert!(validate_branch_name("a/b/c").is_ok());
        assert!(validate_branch_name("my-branch_v2").is_ok());
    }

    #[test]
    fn validate_branch_name_rejects_empty() {
        assert!(validate_branch_name("").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_dot_dot() {
        assert!(validate_branch_name("foo..bar").is_err());
        assert!(validate_branch_name("..").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_invalid_chars() {
        for name in &[
            "foo bar", "foo~bar", "foo^bar", "foo:bar", "foo?bar", "foo*bar", "foo[bar", "foo\\bar",
        ] {
            assert!(
                validate_branch_name(name).is_err(),
                "should reject: {}",
                name
            );
        }
    }

    #[test]
    fn validate_branch_name_rejects_invalid_start_end() {
        assert!(validate_branch_name("/foo").is_err());
        assert!(validate_branch_name("foo/").is_err());
        assert!(validate_branch_name(".foo").is_err());
        assert!(validate_branch_name("foo.").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_consecutive_slashes() {
        assert!(validate_branch_name("foo//bar").is_err());
    }

    // parse_scope_filter tests

    #[test]
    fn scope_filter_none_defaults_to_global_only() {
        assert_eq!(parse_scope_filter(None).unwrap(), ScopeFilter::GlobalOnly);
    }

    #[test]
    fn scope_filter_global_returns_global_only() {
        assert_eq!(
            parse_scope_filter(Some("global")).unwrap(),
            ScopeFilter::GlobalOnly
        );
    }

    #[test]
    fn scope_filter_project_returns_project_and_global() {
        assert_eq!(
            parse_scope_filter(Some("project:my-proj")).unwrap(),
            ScopeFilter::ProjectAndGlobal("my-proj".to_string()),
        );
    }

    #[test]
    fn scope_filter_all_returns_all() {
        assert_eq!(parse_scope_filter(Some("all")).unwrap(), ScopeFilter::All);
    }

    #[test]
    fn scope_filter_invalid_returns_error() {
        assert!(parse_scope_filter(Some("bogus")).is_err());
    }

    // ScopeFilter::matches tests

    #[test]
    fn scope_filter_matches_global_only() {
        let f = ScopeFilter::GlobalOnly;
        assert!(f.matches(&Scope::Global));
        assert!(!f.matches(&Scope::Project("x".into())));
    }

    #[test]
    fn scope_filter_matches_project_and_global() {
        let f = ScopeFilter::ProjectAndGlobal("myproj".into());
        assert!(f.matches(&Scope::Global));
        assert!(f.matches(&Scope::Project("myproj".into())));
        assert!(!f.matches(&Scope::Project("other".into())));
    }

    #[test]
    fn scope_filter_matches_all() {
        let f = ScopeFilter::All;
        assert!(f.matches(&Scope::Global));
        assert!(f.matches(&Scope::Project("anything".into())));
    }
}
