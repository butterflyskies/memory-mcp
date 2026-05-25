use std::{borrow::Cow, fmt, path::Path, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::MemoryError;

use super::validated::{deserialize_validated, ValidatedString};

// ---------------------------------------------------------------------------
// ScopePath newtype
// ---------------------------------------------------------------------------

/// A validated scope path for use in [`Scope::Path`].
///
/// Wraps a `String` that has been validated by [`ValidatedString::validate`]. Once
/// you hold a `ScopePath`, you can use it as `&str` without re-validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ScopePath(String);

impl ValidatedString for ScopePath {
    fn validate(s: &str) -> Result<(), MemoryError> {
        if s.is_empty() {
            return Err(MemoryError::InvalidInput {
                reason: "scope path must not be empty".to_string(),
            });
        }

        if s.starts_with('/') {
            return Err(MemoryError::InvalidInput {
                reason: format!("scope path '{}' must not be an absolute path", s),
            });
        }

        if s.contains('\0') {
            return Err(MemoryError::InvalidInput {
                reason: "scope path must not contain null bytes".to_string(),
            });
        }

        if s.split('/').count() > 10 {
            return Err(MemoryError::InvalidInput {
                reason: format!("scope path '{}' exceeds maximum depth of 10 components", s),
            });
        }

        for component in s.split('/') {
            if component.is_empty() {
                return Err(MemoryError::InvalidInput {
                    reason: format!("scope path '{}' contains an empty component", s),
                });
            }
            if component == ".." {
                return Err(MemoryError::InvalidInput {
                    reason: format!("scope path '{}' contains a '..' component", s),
                });
            }
            if component.starts_with('.') {
                return Err(MemoryError::InvalidInput {
                    reason: format!(
                        "scope path '{}' contains a dot-prefixed component '{}'",
                        s, component
                    ),
                });
            }
            // Reject control characters and characters not suitable in file paths.
            if !component
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                return Err(MemoryError::InvalidInput {
                    reason: format!(
                        "scope path '{}' contains disallowed characters in component '{}'",
                        s, component
                    ),
                });
            }
        }

        Ok(())
    }

    fn wrap(s: String) -> Self {
        Self(s)
    }
}

impl ScopePath {
    /// Validate `s` and wrap it.
    pub fn new(s: impl Into<String>) -> Result<Self, MemoryError> {
        <Self as ValidatedString>::new(s)
    }

    /// Borrow as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derive a `ScopePath` from a filesystem path relative to `namespaces_root`.
    pub fn from_dir(path: &Path, namespaces_root: &Path) -> Result<Self, MemoryError> {
        let relative = path
            .strip_prefix(namespaces_root)
            .map_err(|_| MemoryError::Index("path is not under namespaces root".to_string()))?;
        let s = relative
            .to_str()
            .ok_or_else(|| MemoryError::Index("non-UTF-8 namespace directory path".to_string()))?;
        Self::new(s)
    }
}

impl fmt::Display for ScopePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ScopePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ScopePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_validated(deserializer)
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Where a memory lives on disk and conceptually.
///
/// - `Root`        → `global/`
/// - `Path(p)`     → `projects/{p}/`  (p may contain `/` for hierarchy; on-disk directory stays `projects/` until #256)
///
/// Serialisation uses [`ScopeWire`] so the on-disk/network format is a tagged
/// struct (`{"type":"Root"}` / `{"type":"Path","name":"..."}`) while legacy
/// variants (`"Global"`, `"Project"`) are accepted on deserialisation for
/// backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ScopeWire", into = "ScopeWire")]
#[non_exhaustive]
pub enum Scope {
    /// Machine-wide memories, stored under `global/`.
    Root,
    /// Namespace-scoped memories, stored under `projects/{path}/` on disk.
    ///
    /// The path may contain forward slashes for hierarchy (e.g. `"org/team"`),
    /// and is guaranteed valid by [`ScopePath`]'s constructor.
    Path(ScopePath),
}

/// Wire representation for [`Scope`] serde — handles both current and legacy
/// variant names during deserialisation.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
enum ScopeWire {
    /// Current: global scope.
    Root,
    /// Current: path-scoped memories.
    Path { name: String },
    /// Legacy alias for `Root`.
    Global,
    /// Legacy alias for `Path`.
    Project { name: String },
}

impl TryFrom<ScopeWire> for Scope {
    type Error = MemoryError;

    fn try_from(wire: ScopeWire) -> Result<Self, Self::Error> {
        match wire {
            ScopeWire::Root | ScopeWire::Global => Ok(Scope::Root),
            ScopeWire::Path { name } | ScopeWire::Project { name } => {
                Ok(Scope::Path(ScopePath::new(name)?))
            }
        }
    }
}

impl From<Scope> for ScopeWire {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Root => ScopeWire::Root,
            Scope::Path(sp) => ScopeWire::Path {
                name: sp.as_str().to_owned(),
            },
        }
    }
}

impl Scope {
    /// Directory prefix inside the repo root.
    ///
    /// Returns a `Cow<'static, str>` so `Root` avoids allocation while
    /// `Path` variants produce an owned string.
    pub fn dir_prefix(&self) -> Cow<'static, str> {
        match self {
            Scope::Root => Cow::Borrowed("global"),
            Scope::Path(sp) => Cow::Owned(format!("projects/{}", sp.as_str())),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Root => write!(f, "global"),
            Scope::Path(sp) => write!(f, "{}", sp.as_str()),
        }
    }
}

impl FromStr for Scope {
    type Err = MemoryError;

    /// Parse a scope string.
    ///
    /// | Input | Result |
    /// |---|---|
    /// | `"global"` | `Scope::Root` |
    /// | `"org/team"` | `Scope::Path("org/team")` (bare namespace path) |
    ///
    /// The legacy `"project:{name}"` format is no longer accepted. Use bare
    /// namespace paths instead. Existing memory files with `type: Project` in
    /// YAML frontmatter are still deserialized via [`ScopeWire`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "global" {
            return Ok(Scope::Root);
        }
        Ok(Scope::Path(ScopePath::new(s)?))
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
    /// Search only root (global) memories.
    RootOnly,
    /// Search a subtree: root memories + all memories whose path equals
    /// `prefix` or starts with `prefix/`.
    ///
    /// Uses exact segment matching — `Subtree("eng")` does NOT match
    /// `Path("engineering")`.
    Subtree(ScopePath),
    /// Search all scopes.
    All,
}

impl std::str::FromStr for ScopeFilter {
    type Err = MemoryError;

    /// Parse a scope string into a [`ScopeFilter`] for use in `recall` and `list`.
    ///
    /// | Input | Result |
    /// |---|---|
    /// | `"global"` | `RootOnly` |
    /// | `"org/team"` | `Subtree("org/team")` (bare namespace path) |
    /// | `"all"` | `All` |
    ///
    /// The legacy `"project:{name}"` format is no longer accepted. Use bare
    /// namespace paths instead. For the `None` → `RootOnly` default, use
    /// [`ScopeFilter::parse_or_default`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(ScopeFilter::RootOnly),
            "all" => Ok(ScopeFilter::All),
            s => {
                let parsed = s.parse::<Scope>()?;
                match parsed {
                    Scope::Path(sp) => Ok(ScopeFilter::Subtree(sp)),
                    Scope::Root => Ok(ScopeFilter::RootOnly),
                }
            }
        }
    }
}

impl ScopeFilter {
    /// Parse an optional scope string. `None` defaults to [`ScopeFilter::RootOnly`].
    ///
    /// This is the ergonomic entry point for tool handlers that receive an
    /// `Option<&str>` scope argument.
    pub fn parse_or_default(scope: Option<&str>) -> Result<Self, MemoryError> {
        match scope {
            Some(s) => s.parse(),
            None => Ok(Self::RootOnly),
        }
    }
}

// ---------------------------------------------------------------------------
// Scope path matching
// ---------------------------------------------------------------------------

/// Returns `true` if `path` equals `prefix` exactly or is a direct child
/// path of `prefix` (i.e. starts with `"prefix/"`).
///
/// This uses exact segment matching so `"eng"` matches `"eng"` and `"eng/ml"`
/// but **not** `"engineering"`. Avoids allocating a `format!("{}/", prefix)`
/// string inside hot loops.
pub(crate) fn scope_path_matches(path: &str, prefix: &str) -> bool {
    path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

impl ScopeFilter {
    /// Returns `true` if `scope` should be included given this filter.
    pub fn matches(&self, scope: &Scope) -> bool {
        match self {
            Self::All => true,
            Self::RootOnly => matches!(scope, Scope::Root),
            Self::Subtree(prefix) => match scope {
                Scope::Root => true,
                Scope::Path(sp) => scope_path_matches(sp.as_str(), prefix.as_str()),
            },
        }
    }
}

impl Scope {
    /// Parse an optional scope string. `None` defaults to [`Scope::Root`].
    ///
    /// This is the ergonomic entry point for tool handlers that receive an
    /// `Option<&str>` scope argument.
    pub fn parse_or_default(scope: Option<&str>) -> Result<Self, MemoryError> {
        match scope {
            Some(s) => s.parse(),
            None => Ok(Self::Root),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

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

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
