//! Configuration file parsing for per-scope remote mapping.
//!
//! When a config file exists (`~/.config/memory-mcp/config.toml` or the path
//! in `MEMORY_MCP_CONFIG`), it defines scope-to-repo mappings that route
//! specific scopes to dedicated git repositories with their own remotes.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::info;

use crate::error::MemoryError;
use crate::fs_util::expand_tilde;
use crate::types::ScopePath;

/// A single scope-to-repo mapping from the config file.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct RemoteMapping {
    /// Scope prefix that this mapping captures (e.g. `"work"` or `"org/team"`).
    pub scope: String,
    /// Git remote URL for this scope's repo.
    pub url: String,
    /// Local path for the git repo. Supports `~` expansion.
    /// Defaults to `~/.memory-mcp-{scope}` if omitted.
    pub path: Option<String>,
    /// Branch name for push/pull. Defaults to the server-wide branch if omitted.
    pub branch: Option<String>,
}

/// Top-level config file structure.
#[derive(Debug, Clone, Deserialize, Default)]
#[non_exhaustive]
pub struct Config {
    /// Per-scope remote mappings.
    #[serde(default)]
    pub remotes: Vec<RemoteMapping>,
}

impl Config {
    /// Load config from the given path, returning `Config::default()` if the
    /// file does not exist.
    pub fn load(path: &Path) -> Result<Self, MemoryError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path).map_err(|e| {
            MemoryError::Internal(format!(
                "failed to read config file {}: {}",
                path.display(),
                e
            ))
        })?;
        let config: Config = toml::from_str(&content).map_err(|e| {
            MemoryError::Internal(format!(
                "failed to parse config file {}: {}",
                path.display(),
                e
            ))
        })?;
        for mapping in &config.remotes {
            ScopePath::new(&mapping.scope).map_err(|_| MemoryError::InvalidInput {
                reason: format!(
                    "invalid scope '{}' in config file {}",
                    mapping.scope,
                    path.display()
                ),
            })?;
        }
        info!(
            path = %path.display(),
            remotes = config.remotes.len(),
            "loaded config"
        );
        Ok(config)
    }

    /// Resolve the config file path from the environment or default location.
    ///
    /// Resolution order:
    /// 1. `MEMORY_MCP_CONFIG` environment variable
    /// 2. `~/.config/memory-mcp/config.toml`
    pub fn resolve_path() -> Result<PathBuf, MemoryError> {
        if let Ok(env_path) = std::env::var("MEMORY_MCP_CONFIG") {
            return Ok(PathBuf::from(env_path));
        }
        let config_dir = dirs::config_dir()
            .ok_or_else(|| MemoryError::Internal("could not determine config directory".into()))?;
        Ok(config_dir.join("memory-mcp").join("config.toml"))
    }
}

impl RemoteMapping {
    /// Resolve the local repo path, expanding `~` and applying defaults.
    pub fn resolved_path(&self) -> Result<PathBuf, MemoryError> {
        match &self.path {
            Some(p) => expand_tilde(p),
            None => {
                let home = dirs::home_dir().ok_or_else(|| {
                    MemoryError::Internal("could not determine home directory".into())
                })?;
                let dir_name = format!(".memory-mcp-{}", self.scope.replace('/', "-"));
                Ok(home.join(dir_name))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[[remotes]]
scope = "work"
url = "git@github.com:org/repo.git"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.remotes.len(), 1);
        assert_eq!(config.remotes[0].scope, "work");
        assert_eq!(config.remotes[0].url, "git@github.com:org/repo.git");
        assert!(config.remotes[0].path.is_none());
        assert!(config.remotes[0].branch.is_none());
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[[remotes]]
scope = "work"
url = "git@github.com:org/repo.git"
path = "~/.memory-mcp-work"
branch = "main"

[[remotes]]
scope = "org/team"
url = "git@github.com:org/team-memories.git"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.remotes.len(), 2);
        assert_eq!(
            config.remotes[0].path.as_deref(),
            Some("~/.memory-mcp-work")
        );
        assert_eq!(config.remotes[0].branch.as_deref(), Some("main"));
        assert_eq!(config.remotes[1].scope, "org/team");
    }

    #[test]
    fn empty_config_has_no_remotes() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.remotes.is_empty());
    }

    #[test]
    fn default_path_uses_scope() {
        let mapping = RemoteMapping {
            scope: "work".to_string(),
            url: "https://example.com/repo.git".to_string(),
            path: None,
            branch: None,
        };
        let resolved = mapping.resolved_path().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolved, home.join(".memory-mcp-work"));
    }

    #[test]
    fn default_path_replaces_slashes() {
        let mapping = RemoteMapping {
            scope: "org/team".to_string(),
            url: "https://example.com/repo.git".to_string(),
            path: None,
            branch: None,
        };
        let resolved = mapping.resolved_path().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolved, home.join(".memory-mcp-org-team"));
    }

    #[test]
    fn expand_tilde_home() {
        let result = expand_tilde("~/foo/bar").unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(result, home.join("foo/bar"));
    }

    #[test]
    fn expand_tilde_absolute_passthrough() {
        let result = expand_tilde("/tmp/repo").unwrap();
        assert_eq!(result, PathBuf::from("/tmp/repo"));
    }

    #[test]
    fn expand_tilde_user_rejected() {
        let result = expand_tilde("~otheruser/path");
        assert!(result.is_err());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let config = Config::load(Path::new("/nonexistent/path/config.toml")).unwrap();
        assert!(config.remotes.is_empty());
    }
}
