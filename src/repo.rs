use std::path::{Path, PathBuf};

use git2::{ErrorCode, Repository, Signature};
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    auth::AuthProvider,
    error::MemoryError,
    types::{Memory, Scope},
};

pub struct MemoryRepo {
    inner: Mutex<Repository>,
    root: PathBuf,
}

impl MemoryRepo {
    /// Open an existing git repo at `path`, or initialise a new one.
    pub fn init_or_open(path: &Path) -> Result<Self, MemoryError> {
        let repo = if path.join(".git").exists() {
            Repository::open(path)?
        } else {
            let repo = Repository::init(path)?;
            // Write a .gitignore so the vector index is never committed.
            let gitignore = path.join(".gitignore");
            if !gitignore.exists() {
                std::fs::write(&gitignore, ".memory-mcp-index/\n")?;
            }
            repo
        };
        Ok(Self {
            inner: Mutex::new(repo),
            root: path.to_path_buf(),
        })
    }

    /// Absolute path for a memory's markdown file inside the repo.
    fn memory_path(&self, name: &str, scope: &Scope) -> PathBuf {
        self.root
            .join(scope.dir_prefix())
            .join(format!("{}.md", name))
    }

    /// Write the memory file to disk, then `git add` + `git commit`.
    ///
    /// The file write and commit are performed while holding the Mutex lock
    /// so the entire sequence is atomic with respect to other callers.
    pub async fn save_memory(&self, memory: &Memory) -> Result<(), MemoryError> {
        validate_name(&memory.name)?;
        if let Scope::Project(ref project_name) = memory.metadata.scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(&memory.name, &memory.metadata.scope);
        self.assert_within_root(&file_path)?;

        // Hold the lock for the entire write + commit to avoid TOCTOU.
        let repo = self.inner.lock().await;

        // Ensure the parent directory exists.
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let markdown = memory.to_markdown()?;
        std::fs::write(&file_path, &markdown)?;

        self.git_add_and_commit(
            &repo,
            &file_path,
            &format!("chore: save memory '{}'", memory.name),
        )?;
        Ok(())
    }

    /// Remove a memory's file and commit the deletion.
    pub async fn delete_memory(&self, name: &str, scope: &Scope) -> Result<(), MemoryError> {
        validate_name(name)?;
        if let Scope::Project(ref project_name) = *scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(name, scope);
        self.assert_within_root(&file_path)?;

        let repo = self.inner.lock().await;

        if !file_path.exists() {
            return Err(MemoryError::NotFound {
                name: name.to_string(),
            });
        }

        std::fs::remove_file(&file_path)?;
        // git rm equivalent: stage the removal
        let relative =
            file_path
                .strip_prefix(&self.root)
                .map_err(|e| MemoryError::InvalidInput {
                    reason: format!("path strip error: {}", e),
                })?;
        let mut index = repo.index()?;
        index.remove_path(relative)?;
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(&repo)?;
        let message = format!("chore: delete memory '{}'", name);

        match repo.head() {
            Ok(head) => {
                let parent_commit = head.peel_to_commit()?;
                repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&parent_commit])?;
            }
            Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
                repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[])?;
            }
            Err(e) => return Err(MemoryError::Git(e)),
        }

        Ok(())
    }

    /// Read and parse a memory from disk.
    pub async fn read_memory(&self, name: &str, scope: &Scope) -> Result<Memory, MemoryError> {
        validate_name(name)?;
        if let Scope::Project(ref project_name) = *scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(name, scope);
        self.assert_within_root(&file_path)?;

        if !file_path.exists() {
            return Err(MemoryError::NotFound {
                name: name.to_string(),
            });
        }

        let raw = std::fs::read_to_string(&file_path)?;
        Memory::from_markdown(&raw)
    }

    /// List all memories, optionally filtered by scope.
    pub async fn list_memories(&self, scope: Option<&Scope>) -> Result<Vec<Memory>, MemoryError> {
        let dirs: Vec<PathBuf> = match scope {
            Some(s) => vec![self.root.join(s.dir_prefix())],
            None => {
                // Walk both global/ and projects/*
                let mut dirs = Vec::new();
                let global = self.root.join("global");
                if global.exists() {
                    dirs.push(global);
                }
                let projects = self.root.join("projects");
                if projects.exists() {
                    for entry in std::fs::read_dir(&projects)? {
                        let entry = entry?;
                        if entry.file_type()?.is_dir() {
                            dirs.push(entry.path());
                        }
                    }
                }
                dirs
            }
        };

        let mut memories = Vec::new();
        for dir in dirs {
            if !dir.exists() {
                continue;
            }
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    let raw = std::fs::read_to_string(&path)?;
                    match Memory::from_markdown(&raw) {
                        Ok(m) => memories.push(m),
                        Err(e) => {
                            warn!("skipping {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        Ok(memories)
    }

    /// Push to the configured remote. Stubbed — full implementation is future work.
    pub async fn push(&self, _auth: &AuthProvider) -> Result<(), MemoryError> {
        warn!("push: git remote sync not yet implemented");
        Ok(())
    }

    /// Pull from the configured remote. Stubbed — full implementation is future work.
    pub async fn pull(&self, _auth: &AuthProvider) -> Result<(), MemoryError> {
        warn!("pull: git remote sync not yet implemented");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn signature<'r>(&self, repo: &'r Repository) -> Result<Signature<'r>, MemoryError> {
        // Try repo config first, then fall back to a default.
        let sig = repo
            .signature()
            .or_else(|_| Signature::now("memory-mcp", "memory-mcp@local"))?;
        Ok(sig)
    }

    /// Stage `file_path` and create a commit.
    fn git_add_and_commit(
        &self,
        repo: &Repository,
        file_path: &Path,
        message: &str,
    ) -> Result<(), MemoryError> {
        let relative =
            file_path
                .strip_prefix(&self.root)
                .map_err(|e| MemoryError::InvalidInput {
                    reason: format!("path strip error: {}", e),
                })?;

        let mut index = repo.index()?;
        index.add_path(relative)?;
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(repo)?;

        match repo.head() {
            Ok(head) => {
                let parent_commit = head.peel_to_commit()?;
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent_commit])?;
            }
            Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
                // Initial commit — no parent.
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?;
            }
            Err(e) => return Err(MemoryError::Git(e)),
        }

        Ok(())
    }

    /// Assert that `path` remains under `self.root` after canonicalisation,
    /// preventing path-traversal attacks.
    fn assert_within_root(&self, path: &Path) -> Result<(), MemoryError> {
        // The file may not exist yet, so we canonicalize its parent and
        // then re-append the filename.
        let parent = path.parent().unwrap_or(path);
        let filename = path.file_name().ok_or_else(|| MemoryError::InvalidInput {
            reason: "path has no filename component".to_string(),
        })?;

        // If the parent doesn't exist yet we check as many ancestors as
        // necessary until we find one that does.
        let canon_parent = {
            let mut p = parent.to_path_buf();
            let mut suffixes: Vec<std::ffi::OsString> = Vec::new();
            loop {
                match p.canonicalize() {
                    Ok(c) => {
                        let mut full = c;
                        for s in suffixes.into_iter().rev() {
                            full.push(s);
                        }
                        break full;
                    }
                    Err(_) => {
                        if let Some(name) = p.file_name() {
                            suffixes.push(name.to_os_string());
                        }
                        match p.parent() {
                            Some(par) => p = par.to_path_buf(),
                            None => {
                                // Cannot resolve at all — use the uncanonicalized form.
                                break parent.to_path_buf();
                            }
                        }
                    }
                }
            }
        };

        let resolved = canon_parent.join(filename);

        let canon_root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());

        if !resolved.starts_with(&canon_root) {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "path '{}' escapes repository root '{}'",
                    resolved.display(),
                    canon_root.display()
                ),
            });
        }
        Ok(())
    }
}

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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
