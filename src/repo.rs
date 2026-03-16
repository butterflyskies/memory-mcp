use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use git2::{build::CheckoutBuilder, ErrorCode, MergeOptions, Repository, Signature};
use tracing::{info, warn};

use crate::{
    auth::AuthProvider,
    error::MemoryError,
    types::{validate_name, Memory, PullResult, Scope},
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

/// Build a `RemoteCallbacks` that authenticates with the given token.
///
/// The callbacks live for `'static` because the token is moved in.
fn build_auth_callbacks(token: String) -> git2::RemoteCallbacks<'static> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username, _allowed| {
        git2::Cred::userpass_plaintext("x-access-token", &token)
    });
    callbacks
}

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
        // Resolve the token as a Result<String> so we can move it (Send) into
        // the closure. We defer actually failing until after we've confirmed
        // that origin exists — local-only mode needs no token at all.
        let token_result = auth.resolve_token().map(|t| t.into_inner());
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
        // Resolve the token as a Result<String> so we can move it (Send) into
        // the closure. We defer actually failing until after we've confirmed
        // that origin exists — local-only mode needs no token at all.
        let token_result = auth.resolve_token().map(|t| t.into_inner());
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
                Err(e) => return Err(MemoryError::Git(e)),
            };
            let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;

            // ---- 4. Merge analysis ----------------------------------------------
            let (analysis, _preference) = repo.merge_analysis(&[&fetch_commit])?;

            if analysis.is_up_to_date() {
                info!("pull: already up to date");
                return Ok(PullResult::UpToDate);
            }

            if analysis.is_fast_forward() {
                // Fast-forward: update the branch ref and checkout.
                let refname = format!("refs/heads/{branch}");
                let target_oid = fetch_commit.id();

                match repo.find_reference(&refname) {
                    Ok(mut reference) => {
                        reference.set_target(
                            target_oid,
                            &format!("pull: fast-forward to {}", target_oid),
                        )?;
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
                info!("pull: fast-forwarded to {}", target_oid);
                return Ok(PullResult::FastForward);
            }

            // ---- 5. Normal merge ------------------------------------------------
            let mut merge_opts = MergeOptions::new();
            merge_opts.fail_on_conflict(false);
            repo.merge(&[&fetch_commit], Some(&mut merge_opts), None)?;

            let mut index = repo.index()?;
            let conflicts_resolved = if index.has_conflicts() {
                arc.resolve_conflicts_by_recency(&repo, &mut index)?
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
            let sig = arc.signature(&repo)?;

            let head_commit = repo.head()?.peel_to_commit()?;
            let fetch_commit_obj = repo.find_commit(fetch_commit.id())?;

            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("chore: merge origin/{}", branch),
                &tree,
                &[&head_commit, &fetch_commit_obj],
            )?;

            repo.cleanup_state()?;
            info!(
                "pull: merge complete ({} conflicts auto-resolved)",
                conflicts_resolved
            );
            Ok(PullResult::Merged { conflicts_resolved })
        })
        .await
        .map_err(|e| MemoryError::Join(e.to_string()))?
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
                    (Some(ours), None) => {
                        (ours.as_bytes().to_vec(), "ours (theirs missing)".to_string())
                    }
                    (None, Some(theirs)) => (
                        theirs.as_bytes().to_vec(),
                        "theirs (ours missing)".to_string(),
                    ),
                    (None, None) => {
                        // Both UTF-8 conversions failed — fall back to raw blob bytes.
                        match (
                            info.our_blob.as_deref(),
                            info.their_blob.as_deref(),
                        ) {
                            (Some(ours), _) => (
                                ours.to_vec(),
                                "ours (binary/non-UTF-8)".to_string(),
                            ),
                            (_, Some(theirs)) => (
                                theirs.to_vec(),
                                "theirs (binary/non-UTF-8)".to_string(),
                            ),
                            (None, None) => {
                                // Both blobs truly absent — remove the entry from
                                // the index so write_tree() succeeds.
                                warn!(
                                    "conflict at '{}': both sides missing — removing from index",
                                    info.path.display()
                                );
                                let relative = info.path.strip_prefix(&self.root).map_err(
                                    |e| MemoryError::InvalidInput {
                                        reason: format!(
                                            "path strip error during conflict resolution: {}",
                                            e
                                        ),
                                    },
                                )?;
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
