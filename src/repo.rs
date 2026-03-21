use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use git2::{build::CheckoutBuilder, ErrorCode, MergeOptions, Repository, Signature};
use tracing::{info, warn};

use crate::{
    auth::{AuthProvider, Secret},
    error::MemoryError,
    types::{validate_name, ChangedMemories, Memory, PullResult, Scope},
};

// ---------------------------------------------------------------------------
// Module-level helpers
// ---------------------------------------------------------------------------

/// Strip userinfo (credentials) from a URL before logging.
///
/// `https://user:token@host/path` → `https://[REDACTED]@host/path`
fn redact_url(url: &str) -> String {
    if let Some(at_pos) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let after_at = &url[at_pos + 1..];
            return format!("{}[REDACTED]@{}", scheme, after_at);
        }
    }
    url.to_string()
}

/// Return the current HEAD commit OID as a 20-byte array.
///
/// Returns `[0u8; 20]` as a sentinel when the branch is unborn (no commits yet).
fn capture_head_oid(repo: &git2::Repository) -> Result<[u8; 20], MemoryError> {
    match repo.head() {
        Ok(h) => {
            let oid = h.peel_to_commit()?.id();
            let mut buf = [0u8; 20];
            buf.copy_from_slice(oid.as_bytes());
            Ok(buf)
        }
        // Unborn branch — use zero OID as sentinel.
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            Ok([0u8; 20])
        }
        Err(e) => Err(MemoryError::Git(e)),
    }
}

/// Perform a fast-forward of `fetch_commit` into `branch`.
///
/// Captures the old HEAD OID (zero sentinel if unborn), advances the branch
/// ref, sets HEAD, and force-checks out the new tree.
fn fast_forward(
    repo: &git2::Repository,
    fetch_commit: &git2::AnnotatedCommit,
    branch: &str,
) -> Result<PullResult, MemoryError> {
    let old_head = capture_head_oid(repo)?;

    let refname = format!("refs/heads/{branch}");
    let target_oid = fetch_commit.id();

    match repo.find_reference(&refname) {
        Ok(mut reference) => {
            reference.set_target(target_oid, &format!("pull: fast-forward to {}", target_oid))?;
        }
        Err(e) if e.code() == ErrorCode::NotFound => {
            // Branch doesn't exist locally yet — create it.
            repo.reference(
                &refname,
                target_oid,
                true,
                &format!("pull: create branch {} from fetch", branch),
            )?;
        }
        Err(e) => return Err(MemoryError::Git(e)),
    }

    repo.set_head(&refname)?;
    let mut checkout = CheckoutBuilder::default();
    checkout.force();
    repo.checkout_head(Some(&mut checkout))?;

    let mut new_head = [0u8; 20];
    new_head.copy_from_slice(target_oid.as_bytes());

    info!("pull: fast-forwarded to {}", target_oid);
    Ok(PullResult::FastForward { old_head, new_head })
}

/// Build a `RemoteCallbacks` that authenticates with the given token.
///
/// The callbacks live for `'static` because the token is moved in.
fn build_auth_callbacks(token: Secret<String>) -> git2::RemoteCallbacks<'static> {
    let mut callbacks = git2::RemoteCallbacks::new();
    let raw = token.into_inner();
    callbacks.credentials(move |_url, _username, _allowed| {
        git2::Cred::userpass_plaintext("x-access-token", &raw)
    });
    callbacks
}

/// Git-backed repository for persisting and syncing memory files.
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
    ///
    /// If `remote_url` is provided, ensures an `origin` remote exists pointing
    /// at that URL (creating or updating it as necessary).
    pub fn init_or_open(path: &Path, remote_url: Option<&str>) -> Result<Self, MemoryError> {
        let repo = if path.join(".git").exists() {
            Repository::open(path)?
        } else {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.initial_head("main");
            let repo = Repository::init_opts(path, &opts)?;
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

        // Set up or update the origin remote if a URL was provided.
        if let Some(url) = remote_url {
            match repo.find_remote("origin") {
                Ok(existing) => {
                    // Update the URL only when it differs from the current one.
                    let current_url = existing.url().unwrap_or("");
                    if current_url != url {
                        repo.remote_set_url("origin", url)?;
                        info!("updated origin remote URL to {}", redact_url(url));
                    }
                }
                Err(e) if e.code() == ErrorCode::NotFound => {
                    repo.remote("origin", url)?;
                    info!("created origin remote pointing at {}", redact_url(url));
                }
                Err(e) => return Err(MemoryError::Git(e)),
            }
        }

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
                    // Skip symlinks entirely to prevent directory traversal.
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

    /// Push local commits to `origin/<branch>`.
    ///
    /// If no `origin` remote is configured the call is a no-op (local-only
    /// mode). Auth failures are propagated as `MemoryError::Auth`.
    pub async fn push(
        self: &Arc<Self>,
        auth: &AuthProvider,
        branch: &str,
    ) -> Result<(), MemoryError> {
        // Resolve the token early so we can move it (Send) into the
        // spawn_blocking closure. We defer failing until after we've confirmed
        // that origin exists — local-only mode needs no token at all.
        let token_result = auth.resolve_token();
        let arc = Arc::clone(self);
        let branch = branch.to_string();

        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let repo = arc
                .inner
                .lock()
                .expect("lock poisoned — prior panic corrupted state");

            let mut remote = match repo.find_remote("origin") {
                Ok(r) => r,
                Err(e) if e.code() == ErrorCode::NotFound => {
                    warn!("push: no origin remote configured — skipping (local-only mode)");
                    return Ok(());
                }
                Err(e) => return Err(MemoryError::Git(e)),
            };

            // Origin exists — we need the token now.
            let token = token_result?;
            let callbacks = build_auth_callbacks(token);
            let mut push_opts = git2::PushOptions::new();
            push_opts.remote_callbacks(callbacks);

            let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
            remote.push(&[&refspec], Some(&mut push_opts))?;
            info!("pushed branch '{}' to origin", branch);
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    /// Perform a normal (non-fast-forward) merge of `fetch_commit` into HEAD.
    ///
    /// Resolves any conflicts using recency-based auto-resolution, creates the
    /// merge commit, and cleans up MERGE state.
    fn merge_with_remote(
        &self,
        repo: &git2::Repository,
        fetch_commit: &git2::AnnotatedCommit,
        branch: &str,
    ) -> Result<PullResult, MemoryError> {
        // Capture old HEAD before the merge commit.
        // HEAD must exist here — merge analysis would not reach this path
        // with an unborn branch. Propagate the error if it somehow does.
        let oid = repo.head()?.peel_to_commit()?.id();
        let mut old_head = [0u8; 20];
        old_head.copy_from_slice(oid.as_bytes());

        let mut merge_opts = MergeOptions::new();
        merge_opts.fail_on_conflict(false);
        repo.merge(&[fetch_commit], Some(&mut merge_opts), None)?;

        let mut index = repo.index()?;
        let conflicts_resolved = if index.has_conflicts() {
            self.resolve_conflicts_by_recency(repo, &mut index)?
        } else {
            0
        };

        // Safety check: if any conflicts remain after auto-resolution,
        // clean up the MERGE state and surface a clear error rather than
        // letting write_tree() fail with an opaque message.
        if index.has_conflicts() {
            let _ = repo.cleanup_state();
            return Err(MemoryError::Internal(
                "unresolved conflicts remain after auto-resolution".into(),
            ));
        }

        // Write the merged tree and create the merge commit.
        index.write()?;
        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let sig = self.signature(repo)?;

        let head_commit = repo.head()?.peel_to_commit()?;
        let fetch_commit_obj = repo.find_commit(fetch_commit.id())?;

        let new_commit_oid = repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("chore: merge origin/{}", branch),
            &tree,
            &[&head_commit, &fetch_commit_obj],
        )?;

        repo.cleanup_state()?;

        let mut new_head = [0u8; 20];
        new_head.copy_from_slice(new_commit_oid.as_bytes());

        info!(
            "pull: merge complete ({} conflicts auto-resolved)",
            conflicts_resolved
        );
        Ok(PullResult::Merged {
            conflicts_resolved,
            old_head,
            new_head,
        })
    }

    /// Pull from `origin/<branch>` and merge into the current HEAD.
    ///
    /// Uses a recency-based auto-resolution strategy for conflicts: the version
    /// with the more recent `updated_at` frontmatter timestamp wins. If
    /// timestamps are equal or unparseable, the local version is kept.
    pub async fn pull(
        self: &Arc<Self>,
        auth: &AuthProvider,
        branch: &str,
    ) -> Result<PullResult, MemoryError> {
        // Resolve the token early so we can move it (Send) into the
        // spawn_blocking closure. We defer failing until after we've confirmed
        // that origin exists — local-only mode needs no token at all.
        let token_result = auth.resolve_token();
        let arc = Arc::clone(self);
        let branch = branch.to_string();

        tokio::task::spawn_blocking(move || -> Result<PullResult, MemoryError> {
            let repo = arc
                .inner
                .lock()
                .expect("lock poisoned — prior panic corrupted state");

            // ---- 1. Find origin -------------------------------------------------
            let mut remote = match repo.find_remote("origin") {
                Ok(r) => r,
                Err(e) if e.code() == ErrorCode::NotFound => {
                    warn!("pull: no origin remote configured — skipping (local-only mode)");
                    return Ok(PullResult::NoRemote);
                }
                Err(e) => return Err(MemoryError::Git(e)),
            };

            // Origin exists — we need the token now.
            let token = token_result?;

            // ---- 2. Fetch -------------------------------------------------------
            let callbacks = build_auth_callbacks(token);
            let mut fetch_opts = git2::FetchOptions::new();
            fetch_opts.remote_callbacks(callbacks);
            remote.fetch(&[&branch], Some(&mut fetch_opts), None)?;

            // ---- 3. Resolve FETCH_HEAD ------------------------------------------
            let fetch_head = match repo.find_reference("FETCH_HEAD") {
                Ok(r) => r,
                Err(e) if e.code() == ErrorCode::NotFound => {
                    // Empty remote — nothing to merge.
                    return Ok(PullResult::UpToDate);
                }
                Err(e)
                    if e.class() == git2::ErrorClass::Reference
                        && e.message().contains("corrupted") =>
                {
                    // Empty/corrupted FETCH_HEAD (e.g. remote has no commits yet).
                    info!("pull: FETCH_HEAD is empty or corrupted — treating as empty remote");
                    return Ok(PullResult::UpToDate);
                }
                Err(e) => return Err(MemoryError::Git(e)),
            };
            let fetch_commit = match repo.reference_to_annotated_commit(&fetch_head) {
                Ok(c) => c,
                Err(e) if e.class() == git2::ErrorClass::Reference => {
                    // FETCH_HEAD exists but can't be resolved (empty remote).
                    info!("pull: FETCH_HEAD not resolvable — treating as empty remote");
                    return Ok(PullResult::UpToDate);
                }
                Err(e) => return Err(MemoryError::Git(e)),
            };

            // ---- 4. Merge analysis ----------------------------------------------
            let (analysis, _preference) = repo.merge_analysis(&[&fetch_commit])?;

            if analysis.is_up_to_date() {
                info!("pull: already up to date");
                return Ok(PullResult::UpToDate);
            }

            if analysis.is_fast_forward() {
                return fast_forward(&repo, &fetch_commit, &branch);
            }

            arc.merge_with_remote(&repo, &fetch_commit, &branch)
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
    }

    /// Diff two commits and return the memory files that changed.
    ///
    /// Only `.md` files under `global/` or `projects/` are considered.
    /// Added/modified files go into `upserted`; deleted files go into `removed`.
    /// Qualified names are returned without the `.md` suffix (e.g. `"global/foo"`).
    ///
    /// Must be called from within `spawn_blocking` since it uses git2.
    pub(crate) fn diff_changed_memories(
        &self,
        old_oid: [u8; 20],
        new_oid: [u8; 20],
    ) -> Result<ChangedMemories, MemoryError> {
        let repo = self
            .inner
            .lock()
            .expect("lock poisoned — prior panic corrupted state");

        let new_git_oid = git2::Oid::from_bytes(&new_oid).map_err(MemoryError::Git)?;
        let new_tree = repo.find_commit(new_git_oid)?.tree()?;

        // A zero OID indicates an unborn branch (no prior commits). In that case,
        // diff against an empty tree so all files appear as additions.
        let diff = if old_oid == [0u8; 20] {
            repo.diff_tree_to_tree(None, Some(&new_tree), None)?
        } else {
            let old_git_oid = git2::Oid::from_bytes(&old_oid).map_err(MemoryError::Git)?;
            let old_tree = repo.find_commit(old_git_oid)?.tree()?;
            repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?
        };

        let mut changes = ChangedMemories::default();

        diff.foreach(
            &mut |delta, _progress| {
                use git2::Delta;

                let path = match delta.new_file().path().or_else(|| delta.old_file().path()) {
                    Some(p) => p,
                    None => return true,
                };

                let path_str = match path.to_str() {
                    Some(s) => s,
                    None => return true,
                };

                // Only care about .md files under global/ or projects/
                if !path_str.ends_with(".md") {
                    return true;
                }
                if !path_str.starts_with("global/") && !path_str.starts_with("projects/") {
                    return true;
                }

                // Strip the .md suffix to get the qualified name.
                let qualified = &path_str[..path_str.len() - 3];

                match delta.status() {
                    Delta::Added | Delta::Modified => {
                        changes.upserted.push(qualified.to_string());
                    }
                    Delta::Renamed | Delta::Copied => {
                        // For renames, the old path must be removed from the index
                        // to avoid leaving a ghost vector behind.
                        if matches!(delta.status(), Delta::Renamed) {
                            if let Some(old_path) = delta.old_file().path().and_then(|p| p.to_str())
                            {
                                if old_path.ends_with(".md")
                                    && (old_path.starts_with("global/")
                                        || old_path.starts_with("projects/"))
                                {
                                    changes
                                        .removed
                                        .push(old_path[..old_path.len() - 3].to_string());
                                }
                            }
                        }
                        changes.upserted.push(qualified.to_string());
                    }
                    Delta::Deleted => {
                        changes.removed.push(qualified.to_string());
                    }
                    _ => {}
                }

                true
            },
            None,
            None,
            None,
        )
        .map_err(MemoryError::Git)?;

        Ok(changes)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Resolve all index conflicts using a recency-based strategy.
    ///
    /// For each conflicted entry, the version with the more recent `updated_at`
    /// frontmatter timestamp wins. Ties and parse failures fall back to "ours"
    /// (local). Returns the number of files resolved.
    fn resolve_conflicts_by_recency(
        &self,
        repo: &Repository,
        index: &mut git2::Index,
    ) -> Result<usize, MemoryError> {
        // Collect conflict info first to avoid borrow issues with the index.
        struct ConflictInfo {
            path: PathBuf,
            our_blob: Option<Vec<u8>>,
            their_blob: Option<Vec<u8>>,
        }

        let mut conflicts_info: Vec<ConflictInfo> = Vec::new();

        {
            let conflicts = index.conflicts()?;
            for conflict in conflicts {
                let conflict = conflict?;

                let path = conflict
                    .our
                    .as_ref()
                    .or(conflict.their.as_ref())
                    .and_then(|e| std::str::from_utf8(&e.path).ok())
                    .map(|s| self.root.join(s));

                let path = match path {
                    Some(p) => p,
                    None => continue,
                };

                let our_blob = conflict
                    .our
                    .as_ref()
                    .and_then(|e| repo.find_blob(e.id).ok())
                    .map(|b| b.content().to_vec());

                let their_blob = conflict
                    .their
                    .as_ref()
                    .and_then(|e| repo.find_blob(e.id).ok())
                    .map(|b| b.content().to_vec());

                conflicts_info.push(ConflictInfo {
                    path,
                    our_blob,
                    their_blob,
                });
            }
        }

        let mut resolved = 0usize;

        for info in conflicts_info {
            let our_str = info
                .our_blob
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::to_owned);
            let their_str = info
                .their_blob
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::to_owned);

            let our_ts = our_str
                .as_deref()
                .and_then(|s| Memory::from_markdown(s).ok())
                .map(|m| m.metadata.updated_at);
            let their_ts = their_str
                .as_deref()
                .and_then(|s| Memory::from_markdown(s).ok())
                .map(|m| m.metadata.updated_at);

            // Pick the winning content as raw bytes.
            let (chosen_bytes, label): (Vec<u8>, String) =
                match (our_str.as_deref(), their_str.as_deref()) {
                    (Some(ours), Some(theirs)) => match (our_ts, their_ts) {
                        (Some(ot), Some(tt)) if tt > ot => (
                            theirs.as_bytes().to_vec(),
                            format!("theirs (updated_at: {})", tt),
                        ),
                        (Some(ot), _) => (
                            ours.as_bytes().to_vec(),
                            format!("ours (updated_at: {})", ot),
                        ),
                        _ => (
                            ours.as_bytes().to_vec(),
                            "ours (timestamp unparseable)".to_string(),
                        ),
                    },
                    (Some(ours), None) => (
                        ours.as_bytes().to_vec(),
                        "ours (theirs missing)".to_string(),
                    ),
                    (None, Some(theirs)) => (
                        theirs.as_bytes().to_vec(),
                        "theirs (ours missing)".to_string(),
                    ),
                    (None, None) => {
                        // Both UTF-8 conversions failed — fall back to raw blob bytes.
                        match (info.our_blob.as_deref(), info.their_blob.as_deref()) {
                            (Some(ours), _) => {
                                (ours.to_vec(), "ours (binary/non-UTF-8)".to_string())
                            }
                            (_, Some(theirs)) => {
                                (theirs.to_vec(), "theirs (binary/non-UTF-8)".to_string())
                            }
                            (None, None) => {
                                // Both blobs truly absent — remove the entry from
                                // the index so write_tree() succeeds.
                                warn!(
                                    "conflict at '{}': both sides missing — removing from index",
                                    info.path.display()
                                );
                                let relative = info.path.strip_prefix(&self.root).map_err(|e| {
                                    MemoryError::InvalidInput {
                                        reason: format!(
                                            "path strip error during conflict resolution: {}",
                                            e
                                        ),
                                    }
                                })?;
                                index.conflict_remove(relative)?;
                                resolved += 1;
                                continue;
                            }
                        }
                    }
                };

            warn!(
                "conflict resolved: {} — kept {}",
                info.path.display(),
                label
            );

            // Write the chosen content to the working directory — going through
            // assert_within_root and write_memory_file enforces path-traversal
            // and symlink protections.
            self.assert_within_root(&info.path)?;
            if let Some(parent) = info.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.write_memory_file(&info.path, &chosen_bytes)?;

            // Stage the resolution.
            let relative =
                info.path
                    .strip_prefix(&self.root)
                    .map_err(|e| MemoryError::InvalidInput {
                        reason: format!("path strip error during conflict resolution: {}", e),
                    })?;
            index.add_path(relative)?;

            resolved += 1;
        }

        Ok(resolved)
    }

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthProvider;
    use crate::types::{Memory, MemoryMetadata, PullResult, Scope};
    use std::sync::Arc;

    fn test_auth() -> AuthProvider {
        AuthProvider::with_token("test-token-unused-for-file-remotes")
    }

    fn make_memory(name: &str, content: &str, updated_at_secs: i64) -> Memory {
        let meta = MemoryMetadata {
            tags: vec![],
            scope: Scope::Global,
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            updated_at: chrono::DateTime::from_timestamp(updated_at_secs, 0).unwrap(),
            source: None,
        };
        Memory::new(name.to_string(), content.to_string(), meta)
    }

    fn setup_bare_remote() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        git2::Repository::init_bare(dir.path()).expect("failed to init bare repo");
        let url = format!("file://{}", dir.path().display());
        (dir, url)
    }

    fn open_repo(dir: &tempfile::TempDir, remote_url: Option<&str>) -> Arc<MemoryRepo> {
        Arc::new(MemoryRepo::init_or_open(dir.path(), remote_url).expect("failed to init repo"))
    }

    // -- redact_url tests --------------------------------------------------

    #[test]
    fn redact_url_strips_userinfo() {
        assert_eq!(
            redact_url("https://user:ghp_token123@github.com/org/repo.git"),
            "https://[REDACTED]@github.com/org/repo.git"
        );
    }

    #[test]
    fn redact_url_no_at_passthrough() {
        let url = "https://github.com/org/repo.git";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_file_protocol_passthrough() {
        let url = "file:///tmp/bare.git";
        assert_eq!(redact_url(url), url);
    }

    // -- assert_within_root tests ------------------------------------------

    #[test]
    fn assert_within_root_accepts_valid_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = MemoryRepo::init_or_open(dir.path(), None).unwrap();
        let valid = dir.path().join("global").join("my-memory.md");
        // Create the parent so canonicalization works.
        std::fs::create_dir_all(valid.parent().unwrap()).unwrap();
        assert!(repo.assert_within_root(&valid).is_ok());
    }

    #[test]
    fn assert_within_root_rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let repo = MemoryRepo::init_or_open(dir.path(), None).unwrap();
        // Build a path that escapes the repo root. We need enough ".." to go
        // above the tmpdir, then descend into /tmp/evil.
        let _evil = dir
            .path()
            .join("..")
            .join("..")
            .join("..")
            .join("tmp")
            .join("evil.md");
        // Only assert if the path actually resolves outside root.
        // (If the temp dir is at root level, this might not escape — use an
        // explicit absolute path instead.)
        let outside = std::path::PathBuf::from("/tmp/definitely-outside");
        assert!(repo.assert_within_root(&outside).is_err());
    }

    // -- local-only mode tests (no origin) ---------------------------------

    #[tokio::test]
    async fn push_local_only_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);
        let auth = test_auth();
        // No origin configured — push should silently succeed.
        let result = repo.push(&auth, "main").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn pull_local_only_returns_no_remote() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);
        let auth = test_auth();
        let result = repo.pull(&auth, "main").await.unwrap();
        assert!(matches!(result, PullResult::NoRemote));
    }

    // -- push/pull with local bare remote ----------------------------------

    #[tokio::test]
    async fn push_to_bare_remote() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let local_dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&local_dir, Some(&remote_url));
        let auth = test_auth();

        // Save a memory so there's something to push.
        let mem = make_memory("test-push", "push content", 1_700_000_000);
        repo.save_memory(&mem).await.unwrap();

        // Push should succeed.
        repo.push(&auth, "main").await.unwrap();

        // Verify the bare repo received the commit.
        let bare = git2::Repository::open_bare(_remote_dir.path()).unwrap();
        let head = bare.find_reference("refs/heads/main").unwrap();
        let commit = head.peel_to_commit().unwrap();
        assert!(commit.message().unwrap().contains("test-push"));
    }

    #[tokio::test]
    async fn pull_from_empty_bare_remote_returns_up_to_date() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let local_dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&local_dir, Some(&remote_url));
        let auth = test_auth();

        // First save something locally so we have an initial commit (HEAD exists).
        let mem = make_memory("seed", "seed content", 1_700_000_000);
        repo.save_memory(&mem).await.unwrap();

        // Pull from empty remote — should be up-to-date (not an error).
        let result = repo.pull(&auth, "main").await.unwrap();
        assert!(matches!(result, PullResult::UpToDate));
    }

    #[tokio::test]
    async fn pull_fast_forward() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let auth = test_auth();

        // Repo A: save and push
        let dir_a = tempfile::tempdir().unwrap();
        let repo_a = open_repo(&dir_a, Some(&remote_url));
        let mem = make_memory("from-a", "content from A", 1_700_000_000);
        repo_a.save_memory(&mem).await.unwrap();
        repo_a.push(&auth, "main").await.unwrap();

        // Repo B: init with same remote, then pull
        let dir_b = tempfile::tempdir().unwrap();
        let repo_b = open_repo(&dir_b, Some(&remote_url));
        // Repo B needs an initial commit for HEAD to exist.
        let seed = make_memory("seed-b", "seed", 1_700_000_000);
        repo_b.save_memory(&seed).await.unwrap();

        let result = repo_b.pull(&auth, "main").await.unwrap();
        assert!(
            matches!(
                result,
                PullResult::FastForward { .. } | PullResult::Merged { .. }
            ),
            "expected fast-forward or merge, got {:?}",
            result
        );

        // Verify the memory file from A exists in B's working directory.
        let file = dir_b.path().join("global").join("from-a.md");
        assert!(file.exists(), "from-a.md should exist in repo B after pull");
    }

    #[tokio::test]
    async fn pull_up_to_date_after_push() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let local_dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&local_dir, Some(&remote_url));
        let auth = test_auth();

        let mem = make_memory("synced", "synced content", 1_700_000_000);
        repo.save_memory(&mem).await.unwrap();
        repo.push(&auth, "main").await.unwrap();

        // Pull immediately after push — should be up to date.
        let result = repo.pull(&auth, "main").await.unwrap();
        assert!(matches!(result, PullResult::UpToDate));
    }

    // -- conflict resolution tests -----------------------------------------

    #[tokio::test]
    async fn pull_merge_conflict_theirs_newer_wins() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let auth = test_auth();

        // Repo A: save "shared" with T1, push
        let dir_a = tempfile::tempdir().unwrap();
        let repo_a = open_repo(&dir_a, Some(&remote_url));
        let mem_a1 = make_memory("shared", "version from A initial", 1_700_000_100);
        repo_a.save_memory(&mem_a1).await.unwrap();
        repo_a.push(&auth, "main").await.unwrap();

        // Repo B: pull to get A's commit, then modify "shared" with T3 (newer), push
        let dir_b = tempfile::tempdir().unwrap();
        let repo_b = open_repo(&dir_b, Some(&remote_url));
        let seed = make_memory("seed-b", "seed", 1_700_000_000);
        repo_b.save_memory(&seed).await.unwrap();
        repo_b.pull(&auth, "main").await.unwrap();

        let mem_b = make_memory("shared", "version from B (newer)", 1_700_000_300);
        repo_b.save_memory(&mem_b).await.unwrap();
        repo_b.push(&auth, "main").await.unwrap();

        // Repo A: modify "shared" with T2 (older than T3), then pull — conflict
        let mem_a2 = make_memory("shared", "version from A (older)", 1_700_000_200);
        repo_a.save_memory(&mem_a2).await.unwrap();
        let result = repo_a.pull(&auth, "main").await.unwrap();

        assert!(
            matches!(result, PullResult::Merged { conflicts_resolved, .. } if conflicts_resolved >= 1),
            "expected merge with conflicts resolved, got {:?}",
            result
        );

        // Verify theirs (B's version, T3=300) won.
        let file = dir_a.path().join("global").join("shared.md");
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(
            content.contains("version from B (newer)"),
            "expected B's version to win (newer timestamp), got: {}",
            content
        );
    }

    #[tokio::test]
    async fn pull_merge_conflict_ours_newer_wins() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let auth = test_auth();

        // Repo A: save "shared" with T1, push
        let dir_a = tempfile::tempdir().unwrap();
        let repo_a = open_repo(&dir_a, Some(&remote_url));
        let mem_a1 = make_memory("shared", "version from A initial", 1_700_000_100);
        repo_a.save_memory(&mem_a1).await.unwrap();
        repo_a.push(&auth, "main").await.unwrap();

        // Repo B: pull, modify with T2 (older), push
        let dir_b = tempfile::tempdir().unwrap();
        let repo_b = open_repo(&dir_b, Some(&remote_url));
        let seed = make_memory("seed-b", "seed", 1_700_000_000);
        repo_b.save_memory(&seed).await.unwrap();
        repo_b.pull(&auth, "main").await.unwrap();

        let mem_b = make_memory("shared", "version from B (older)", 1_700_000_200);
        repo_b.save_memory(&mem_b).await.unwrap();
        repo_b.push(&auth, "main").await.unwrap();

        // Repo A: modify with T3 (newer), pull — conflict
        let mem_a2 = make_memory("shared", "version from A (newer)", 1_700_000_300);
        repo_a.save_memory(&mem_a2).await.unwrap();
        let result = repo_a.pull(&auth, "main").await.unwrap();

        assert!(
            matches!(result, PullResult::Merged { conflicts_resolved, .. } if conflicts_resolved >= 1),
            "expected merge with conflicts resolved, got {:?}",
            result
        );

        // Verify ours (A's version, T3=300) won.
        let file = dir_a.path().join("global").join("shared.md");
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(
            content.contains("version from A (newer)"),
            "expected A's version to win (newer timestamp), got: {}",
            content
        );
    }

    #[tokio::test]
    async fn pull_merge_no_conflict_different_files() {
        let (_remote_dir, remote_url) = setup_bare_remote();
        let auth = test_auth();

        // Repo A: save "mem-a", push
        let dir_a = tempfile::tempdir().unwrap();
        let repo_a = open_repo(&dir_a, Some(&remote_url));
        let mem_a = make_memory("mem-a", "from A", 1_700_000_100);
        repo_a.save_memory(&mem_a).await.unwrap();
        repo_a.push(&auth, "main").await.unwrap();

        // Repo B: pull, save "mem-b", push
        let dir_b = tempfile::tempdir().unwrap();
        let repo_b = open_repo(&dir_b, Some(&remote_url));
        let seed = make_memory("seed-b", "seed", 1_700_000_000);
        repo_b.save_memory(&seed).await.unwrap();
        repo_b.pull(&auth, "main").await.unwrap();
        let mem_b = make_memory("mem-b", "from B", 1_700_000_200);
        repo_b.save_memory(&mem_b).await.unwrap();
        repo_b.push(&auth, "main").await.unwrap();

        // Repo A: save "mem-a2" (different file), pull — should merge cleanly
        let mem_a2 = make_memory("mem-a2", "also from A", 1_700_000_300);
        repo_a.save_memory(&mem_a2).await.unwrap();
        let result = repo_a.pull(&auth, "main").await.unwrap();

        assert!(
            matches!(
                result,
                PullResult::Merged {
                    conflicts_resolved: 0,
                    ..
                }
            ),
            "expected clean merge, got {:?}",
            result
        );

        // Both repos should have all files.
        assert!(dir_a.path().join("global").join("mem-b.md").exists());
    }

    // -- diff_changed_memories tests ----------------------------------------

    /// Helper: commit a file with given content and return the new HEAD OID bytes.
    fn commit_file(repo: &Arc<MemoryRepo>, rel_path: &str, content: &str) -> [u8; 20] {
        let inner = repo.inner.lock().expect("lock poisoned");
        let full_path = repo.root.join(rel_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full_path, content).unwrap();

        let mut index = inner.index().unwrap();
        index.add_path(std::path::Path::new(rel_path)).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = inner.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();

        let oid = match inner.head() {
            Ok(head) => {
                let parent = head.peel_to_commit().unwrap();
                inner
                    .commit(Some("HEAD"), &sig, &sig, "test commit", &tree, &[&parent])
                    .unwrap()
            }
            Err(_) => inner
                .commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .unwrap(),
        };

        let mut buf = [0u8; 20];
        buf.copy_from_slice(oid.as_bytes());
        buf
    }

    #[test]
    fn diff_changed_memories_detects_added_global() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);

        // Capture the initial HEAD (init commit).
        let old_oid = {
            let inner = repo.inner.lock().unwrap();
            let head = inner.head().unwrap();
            let mut buf = [0u8; 20];
            buf.copy_from_slice(head.peel_to_commit().unwrap().id().as_bytes());
            buf
        };

        let new_oid = commit_file(&repo, "global/my-note.md", "# content");

        let changes = repo.diff_changed_memories(old_oid, new_oid).unwrap();
        assert_eq!(changes.upserted, vec!["global/my-note".to_string()]);
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn diff_changed_memories_detects_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);

        let first_oid = commit_file(&repo, "global/to-delete.md", "hello");
        let second_oid = {
            let inner = repo.inner.lock().unwrap();
            let full_path = dir.path().join("global/to-delete.md");
            std::fs::remove_file(&full_path).unwrap();
            let mut index = inner.index().unwrap();
            index
                .remove_path(std::path::Path::new("global/to-delete.md"))
                .unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = inner.find_tree(tree_oid).unwrap();
            let sig = git2::Signature::now("test", "test@test.com").unwrap();
            let parent = inner.head().unwrap().peel_to_commit().unwrap();
            let oid = inner
                .commit(Some("HEAD"), &sig, &sig, "delete file", &tree, &[&parent])
                .unwrap();
            let mut buf = [0u8; 20];
            buf.copy_from_slice(oid.as_bytes());
            buf
        };

        let changes = repo.diff_changed_memories(first_oid, second_oid).unwrap();
        assert!(changes.upserted.is_empty());
        assert_eq!(changes.removed, vec!["global/to-delete".to_string()]);
    }

    #[test]
    fn diff_changed_memories_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);

        let old_oid = {
            let inner = repo.inner.lock().unwrap();
            let mut buf = [0u8; 20];
            buf.copy_from_slice(
                inner
                    .head()
                    .unwrap()
                    .peel_to_commit()
                    .unwrap()
                    .id()
                    .as_bytes(),
            );
            buf
        };

        // Add a non-.md file under global/ and a .md file outside tracked dirs.
        let _ = commit_file(&repo, "global/config.json", "{}");
        let new_oid = commit_file(&repo, "other/note.md", "# ignored");

        let changes = repo.diff_changed_memories(old_oid, new_oid).unwrap();
        assert!(
            changes.upserted.is_empty(),
            "should ignore non-.md and out-of-scope files"
        );
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn diff_changed_memories_detects_modified() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);

        let first_oid = commit_file(&repo, "projects/myproject/note.md", "version 1");
        let second_oid = commit_file(&repo, "projects/myproject/note.md", "version 2");

        let changes = repo.diff_changed_memories(first_oid, second_oid).unwrap();
        assert_eq!(
            changes.upserted,
            vec!["projects/myproject/note".to_string()]
        );
        assert!(changes.removed.is_empty());
    }

    /// A zero OID (unborn branch sentinel) must not crash; all files in the
    /// new commit should appear as additions.
    #[test]
    fn diff_changed_memories_zero_oid_treats_all_as_added() {
        let dir = tempfile::tempdir().unwrap();
        let repo = open_repo(&dir, None);

        // Commit a global memory file — this is the "new" state.
        let new_oid = commit_file(&repo, "global/first-memory.md", "# Hello");

        // old_oid = [0u8; 20] simulates an unborn branch (no prior commit).
        let old_oid = [0u8; 20];

        let changes = repo.diff_changed_memories(old_oid, new_oid).unwrap();
        assert_eq!(
            changes.upserted,
            vec!["global/first-memory".to_string()],
            "zero OID: all new-tree files should be additions"
        );
        assert!(changes.removed.is_empty(), "zero OID: no removals expected");
    }
}
