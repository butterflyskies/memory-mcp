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

    for component in name.split('/') {
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

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Where a memory lives on disk and conceptually.
///
/// - `Global`           → `global/`
/// - `Project(name)`    → `projects/{name}/`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "name")]
pub enum Scope {
    Global,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub tags: Vec<String>,
    pub scope: Scope,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Optional hint about where this memory came from (e.g. a tool name).
    pub source: Option<String>,
}

impl MemoryMetadata {
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
    pub metadata: MemoryMetadata,
}

impl Memory {
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
    /// Scope: `"global"` or `"project:{name}"`. Defaults to `"global"`.
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
    /// Scope filter: `"global"`, `"project:{name}"`, or omit for all.
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
    /// Scope of the memory. Defaults to "global".
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
    /// Scope of the memory. Defaults to "global".
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `list` tool — browse stored memories.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    /// Scope filter: `"global"`, `"project:{name}"`, or omit for all.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Arguments for the `read` tool — retrieve a specific memory by name.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// Exact name of the memory to read.
    pub name: String,
    /// Scope of the memory. Defaults to "global".
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
// AppState
// ---------------------------------------------------------------------------

use std::sync::Arc;

use crate::{auth::AuthProvider, embedding::EmbeddingEngine, index::VectorIndex, repo::MemoryRepo};

/// Shared application state threaded through the Axum server.
///
/// Wrapped in a single outer `Arc` at the call site. `repo` is additionally
/// wrapped in its own `Arc` so it can be cloned into `spawn_blocking` closures.
pub struct AppState {
    pub repo: Arc<MemoryRepo>,
    pub embedding: EmbeddingEngine,
    pub index: VectorIndex,
    pub auth: AuthProvider,
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
}
