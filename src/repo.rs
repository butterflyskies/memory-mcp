use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use git2::{ErrorCode, Repository, Signature};
use tracing::warn;

use crate::{
    auth::AuthProvider,
    error::MemoryError,
    types::{validate_name, Memory, Scope},
};

pub struct MemoryRepo {
    inner: Mutex<Repository>,
    root: PathBuf,
}

// SAFETY: Repository holds raw pointers but is documented as safe to send
// across threads when not used concurrently. We guarantee exclusive access via
// the Mutex, so MemoryRepo is Send + Sync.
unsafe impl Send for MemoryRepo {}
unsafe impl Sync for MemoryRepo {}

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
            // Commit .gitignore as the initial commit.
            {
                let mut index = repo.index()?;
                index.add_path(Path::new(".gitignore"))?;
                index.write()?;
                let tree_oid = index.write_tree()?;
                let tree = repo.find_tree(tree_oid)?;
                let sig = Signature::now("memory-mcp", "memory-mcp@local")?;
                repo.commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    "chore: init repository",
                    &tree,
                    &[],
                )?;
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
    /// All blocking work (mutex lock + fs ops + git2 ops) is performed inside
    /// `tokio::task::spawn_blocking` so the async executor is not stalled.
    pub async fn save_memory(self: &Arc<Self>, memory: &Memory) -> Result<(), MemoryError> {
        validate_name(&memory.name)?;
        if let Scope::Project(ref project_name) = memory.metadata.scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(&memory.name, &memory.metadata.scope);
        self.assert_within_root(&file_path)?;

        let arc = Arc::clone(self);
        let memory = memory.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let repo = arc
                .inner
                .lock()
                .expect("lock poisoned — prior panic corrupted state");

            // Ensure the parent directory exists.
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let markdown = memory.to_markdown()?;
            arc.write_memory_file(&file_path, markdown.as_bytes())?;

            arc.git_add_and_commit(
                &repo,
                &file_path,
                &format!("chore: save memory '{}'", memory.name),
            )?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    /// Remove a memory's file and commit the deletion.
    pub async fn delete_memory(
        self: &Arc<Self>,
        name: &str,
        scope: &Scope,
    ) -> Result<(), MemoryError> {
        validate_name(name)?;
        if let Scope::Project(ref project_name) = *scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(name, scope);
        self.assert_within_root(&file_path)?;

        let arc = Arc::clone(self);
        let name = name.to_string();
        let file_path_clone = file_path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let repo = arc
                .inner
                .lock()
                .expect("lock poisoned — prior panic corrupted state");

            // Check existence and symlink status atomically via symlink_metadata.
            match std::fs::symlink_metadata(&file_path_clone) {
                Err(_) => return Err(MemoryError::NotFound { name: name.clone() }),
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(MemoryError::InvalidInput {
                        reason: format!(
                            "path '{}' is a symlink, which is not permitted",
                            file_path_clone.display()
                        ),
                    });
                }
                Ok(_) => {}
            }

            std::fs::remove_file(&file_path_clone)?;
            // git rm equivalent: stage the removal
            let relative =
                file_path_clone
                    .strip_prefix(&arc.root)
                    .map_err(|e| MemoryError::InvalidInput {
                        reason: format!("path strip error: {}", e),
                    })?;
            let mut index = repo.index()?;
            index.remove_path(relative)?;
            index.write()?;

            let tree_oid = index.write_tree()?;
            let tree = repo.find_tree(tree_oid)?;
            let sig = arc.signature(&repo)?;
            let message = format!("chore: delete memory '{}'", name);

            match repo.head() {
                Ok(head) => {
                    let parent_commit = head.peel_to_commit()?;
                    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&parent_commit])?;
                }
                Err(e)
                    if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound =>
                {
                    repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &[])?;
                }
                Err(e) => return Err(MemoryError::Git(e)),
            }

            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    /// Read and parse a memory from disk.
    pub async fn read_memory(
        self: &Arc<Self>,
        name: &str,
        scope: &Scope,
    ) -> Result<Memory, MemoryError> {
        validate_name(name)?;
        if let Scope::Project(ref project_name) = *scope {
            validate_name(project_name)?;
        }

        let file_path = self.memory_path(name, scope);
        self.assert_within_root(&file_path)?;

        let arc = Arc::clone(self);
        let name = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Memory, MemoryError> {
            // Check existence/symlink status before opening.
            match std::fs::symlink_metadata(&file_path) {
                Err(_) => return Err(MemoryError::NotFound { name }),
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(MemoryError::InvalidInput {
                        reason: format!(
                            "path '{}' is a symlink, which is not permitted",
                            file_path.display()
                        ),
                    });
                }
                Ok(_) => {}
            }
            let raw = arc.read_memory_file(&file_path)?;
            Memory::from_markdown(&raw)
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    /// List all memories, optionally filtered by scope.
    pub async fn list_memories(
        self: &Arc<Self>,
        scope: Option<&Scope>,
    ) -> Result<Vec<Memory>, MemoryError> {
        let root = self.root.clone();
        let scope_clone = scope.cloned();

        tokio::task::spawn_blocking(move || -> Result<Vec<Memory>, MemoryError> {
            let dirs: Vec<PathBuf> = match scope_clone.as_ref() {
                Some(s) => vec![root.join(s.dir_prefix())],
                None => {
                    // Walk both global/ and projects/*
                    let mut dirs = Vec::new();
                    let global = root.join("global");
                    if global.exists() {
                        dirs.push(global);
                    }
                    let projects = root.join("projects");
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

            fn collect_md_files(dir: &Path, out: &mut Vec<Memory>) -> Result<(), MemoryError> {
                if !dir.exists() {
                    return Ok(());
                }
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    let ft = entry.file_type()?;
                    // Fix 1: skip symlinks entirely to prevent directory traversal.
                    if ft.is_symlink() {
                        warn!(
                            "skipping symlink at {:?} — symlinks are not permitted in the memory store",
                            path
                        );
                        continue;
                    }
                    if ft.is_dir() {
                        collect_md_files(&path, out)?;
                    } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                        let raw = std::fs::read_to_string(&path)?;
                        match Memory::from_markdown(&raw) {
                            Ok(m) => out.push(m),
                            Err(e) => {
                                warn!("skipping {:?}: {}", path, e);
                            }
                        }
                    }
                }
                Ok(())
            }

            let mut memories = Vec::new();
            for dir in dirs {
                collect_md_files(&dir, &mut memories)?;
            }

            Ok(memories)
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
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
                                return Err(MemoryError::InvalidInput {
                                    reason: "cannot resolve any ancestor of path".into(),
                                });
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
            .map_err(|e| MemoryError::InvalidInput {
                reason: format!("cannot canonicalize repo root: {}", e),
            })?;

        if !resolved.starts_with(&canon_root) {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "path '{}' escapes repository root '{}'",
                    resolved.display(),
                    canon_root.display()
                ),
            });
        }

        // Reject any symlinks within the repo root. We check each existing
        // component of `resolved` that lies inside `canon_root` — if any is a
        // symlink the request is rejected, because canonicalization already
        // followed it and the prefix check above would silently pass.
        {
            let mut probe = canon_root.clone();
            // Collect the path components that are beneath the root.
            let relative =
                resolved
                    .strip_prefix(&canon_root)
                    .map_err(|e| MemoryError::InvalidInput {
                        reason: format!("path strip error: {}", e),
                    })?;
            for component in relative.components() {
                probe.push(component);
                // Only check components that currently exist on disk.
                if (probe.exists() || probe.symlink_metadata().is_ok())
                    && probe
                        .symlink_metadata()
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false)
                {
                    return Err(MemoryError::InvalidInput {
                        reason: format!(
                            "path component '{}' is a symlink, which is not allowed",
                            probe.display()
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Open `path` for writing using `O_NOFOLLOW` on Unix so the final path
    /// component cannot be a symlink, then write `data`.
    ///
    /// On non-Unix platforms falls back to a plain `std::fs::write`.
    fn write_memory_file(&self, path: &Path, data: &[u8]) -> Result<(), MemoryError> {
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            f.write_all(data)?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            std::fs::write(path, data)?;
            Ok(())
        }
    }

    /// Open `path` for reading using `O_NOFOLLOW` on Unix, then return its
    /// contents as a `String`.
    ///
    /// On non-Unix platforms falls back to `std::fs::read_to_string`.
    fn read_memory_file(&self, path: &Path) -> Result<String, MemoryError> {
        #[cfg(unix)]
        {
            use std::io::Read as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            let mut buf = String::new();
            f.read_to_string(&mut buf)?;
            Ok(buf)
        }
        #[cfg(not(unix))]
        {
            Ok(std::fs::read_to_string(path)?)
        }
    }
}
