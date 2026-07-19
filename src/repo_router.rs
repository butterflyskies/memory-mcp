//! Routes memory operations to the correct [`crate::repo::MemoryRepo`] based on scope.
//!
//! When per-scope remote mappings are configured, each mapped scope prefix
//! gets its own independent git repository. Unmapped scopes fall through to
//! the default repo. Read operations aggregate across all repos.

use std::sync::Arc;

use tracing::{info, warn};

use crate::{
    auth::AuthProvider,
    error::MemoryError,
    health::SubsystemReporter,
    repo::MemoryRepo,
    types::{ChangedMemories, Memory, MemoryName, PullResult, ResolvedChanges, Scope},
};

/// A scope-to-repo entry in the router.
#[derive(Clone)]
struct ScopeRoute {
    /// Scope prefix this route captures.
    prefix: String,
    /// The git repository for this scope.
    repo: Arc<MemoryRepo>,
    /// Branch name for push/pull (overrides the server-wide default).
    branch: Option<String>,
}

/// Routes memory operations to scope-specific git repositories.
///
/// Holds a default repo for unmapped scopes and zero or more scope-specific
/// repos. Write operations route to the repo that owns the scope. Read
/// operations aggregate across all repos.
#[derive(Clone)]
pub struct RepoRouter {
    /// The default repo for scopes that don't match any configured mapping.
    default_repo: Arc<MemoryRepo>,
    /// Scope-specific repos, ordered by prefix length descending so longest
    /// prefix matches first.
    routes: Vec<ScopeRoute>,
}

/// Result of a sync (push/pull) across all repos.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct MultiSyncResult {
    /// Per-repo sync outcomes, one entry per repo (default + each route).
    pub results: Vec<SyncEntry>,
}

impl MultiSyncResult {
    /// `true` when every repo's pull and push completed without error.
    pub fn all_ok(&self) -> bool {
        self.results.iter().all(SyncEntry::is_ok)
    }
}

/// Outcome of syncing a single repo.
#[derive(Debug)]
#[non_exhaustive]
pub struct SyncEntry {
    /// Human-readable label (`"default"` or the scope prefix).
    pub label: String,
    /// The scope used to resolve this repo (for reindex routing).
    pub scope: Scope,
    /// Pull result when the pull was performed and succeeded.
    pub pull: Option<PullResult>,
    /// Error message when the pull was attempted and failed.
    pub pull_error: Option<String>,
    /// Whether push succeeded. `true` in local-only mode (no remote means
    /// nothing to push); `false` when the push failed or was not attempted
    /// because the pull failed first.
    pub push_ok: bool,
    /// Error message when the push was attempted and failed.
    pub push_error: Option<String>,
    /// Memories that changed during pull (for incremental reindex).
    pub changes: Option<ChangedMemories>,
    /// Structured changed memories used by derived-index mirrors.
    pub(crate) resolved_changes: Option<ResolvedChanges>,
    /// Whether post-pull change discovery completed without gaps.
    pub(crate) changes_complete: bool,
}

impl SyncEntry {
    /// `true` when neither the pull nor the push recorded an error.
    pub fn is_ok(&self) -> bool {
        self.pull_error.is_none() && self.push_error.is_none()
    }

    /// Describe the recorded pull/push errors, or `None` when the entry
    /// completed without error.
    pub fn failure_summary(&self) -> Option<String> {
        let mut ops = Vec::new();
        if let Some(e) = &self.pull_error {
            ops.push(format!("pull failed: {e}"));
        }
        if let Some(e) = &self.push_error {
            ops.push(format!("push failed: {e}"));
        }
        if ops.is_empty() {
            None
        } else {
            Some(ops.join("; "))
        }
    }
}

impl RepoRouter {
    /// Create a router with only a default repo (no scope mappings).
    pub fn single(default_repo: Arc<MemoryRepo>) -> Self {
        Self {
            default_repo,
            routes: Vec::new(),
        }
    }

    /// Create a router from config, initialising scope-specific repos.
    ///
    /// Each `RemoteMapping` in the config produces a new `MemoryRepo` at the
    /// resolved path with the mapping's URL as origin.
    pub fn from_config(
        default_repo: Arc<MemoryRepo>,
        mappings: &[crate::config::RemoteMapping],
        git_reporter: &SubsystemReporter,
        sync_reporter: &SubsystemReporter,
    ) -> Result<Self, MemoryError> {
        let mut routes = Vec::with_capacity(mappings.len());
        for mapping in mappings {
            let path = mapping.resolved_path()?;
            info!(
                scope = %mapping.scope,
                path = %path.display(),
                url = %mapping.url,
                "initialising scope-specific repo"
            );
            let repo = MemoryRepo::init_or_open_with_reporter(
                &path,
                Some(&mapping.url),
                git_reporter.clone(),
                sync_reporter.clone(),
            )?;
            routes.push(ScopeRoute {
                prefix: mapping.scope.clone(),
                repo: Arc::new(repo),
                branch: mapping.branch.clone(),
            });
        }
        // Sort by prefix length descending so longest match wins.
        routes.sort_by_key(|r| std::cmp::Reverse(r.prefix.len()));
        Ok(Self {
            default_repo,
            routes,
        })
    }

    /// Find the repo that owns a given scope.
    fn repo_for_scope(&self, scope: &Scope) -> &Arc<MemoryRepo> {
        match scope {
            Scope::Root => &self.default_repo,
            Scope::Path(sp) => {
                let path = sp.as_str();
                for route in &self.routes {
                    if path == route.prefix
                        || (path.starts_with(&route.prefix)
                            && path.as_bytes().get(route.prefix.len()) == Some(&b'/'))
                    {
                        return &route.repo;
                    }
                }
                &self.default_repo
            }
        }
    }

    /// Resolve the branch name for a given scope.
    #[cfg(test)]
    fn branch_for_scope(&self, scope: &Scope, default_branch: &str) -> String {
        match scope {
            Scope::Root => default_branch.to_string(),
            Scope::Path(sp) => {
                let path = sp.as_str();
                for route in &self.routes {
                    if path == route.prefix
                        || (path.starts_with(&route.prefix)
                            && path.as_bytes().get(route.prefix.len()) == Some(&b'/'))
                    {
                        return route
                            .branch
                            .as_deref()
                            .unwrap_or(default_branch)
                            .to_string();
                    }
                }
                default_branch.to_string()
            }
        }
    }

    /// Get a reference to the default repo.
    pub fn default_repo(&self) -> &Arc<MemoryRepo> {
        &self.default_repo
    }

    /// Iterate over all repos (default + scope-mapped).
    fn all_repos(&self) -> impl Iterator<Item = (&str, &Arc<MemoryRepo>, Option<&str>, Scope)> {
        std::iter::once(("default", &self.default_repo, None, Scope::Root)).chain(
            self.routes.iter().map(|r| {
                let scope = crate::types::ScopePath::new(&r.prefix)
                    .map(Scope::Path)
                    .expect("scope prefix validated at config load");
                (r.prefix.as_str(), &r.repo, r.branch.as_deref(), scope)
            }),
        )
    }

    // -----------------------------------------------------------------------
    // Write operations — route to the correct repo
    // -----------------------------------------------------------------------

    /// Save a memory to the repo that owns its scope.
    pub async fn save_memory(&self, memory: &Memory) -> Result<(), MemoryError> {
        let repo = self.repo_for_scope(&memory.metadata.scope);
        repo.save_memory(memory).await
    }

    /// Delete a memory from the repo that owns its scope.
    pub async fn delete_memory(&self, name: &str, scope: &Scope) -> Result<(), MemoryError> {
        let repo = self.repo_for_scope(scope);
        repo.delete_memory(name, scope).await
    }

    /// Read a memory from the repo that owns its scope.
    pub async fn read_memory(&self, name: &str, scope: &Scope) -> Result<Memory, MemoryError> {
        let repo = self.repo_for_scope(scope);
        repo.read_memory(name, scope).await
    }

    /// Move a memory, possibly across repos.
    pub async fn move_memory(
        &self,
        source_name: &str,
        source_scope: &Scope,
        dest_name: &MemoryName,
        dest_scope: &Scope,
    ) -> Result<Memory, MemoryError> {
        let source_repo = self.repo_for_scope(source_scope);
        let dest_repo = self.repo_for_scope(dest_scope);

        if Arc::ptr_eq(source_repo, dest_repo) {
            // Same repo — use the atomic move.
            source_repo
                .move_memory(source_name, source_scope, dest_name, dest_scope)
                .await
        } else {
            // Cross-repo move: read from source, save to dest, delete from source.
            // Preserve id and created_at from the source so recall_log references
            // and memory identity survive the move.
            let source = source_repo.read_memory(source_name, source_scope).await?;
            let metadata = crate::types::MemoryMetadata {
                scope: dest_scope.clone(),
                tags: source.metadata.tags.clone(),
                source: source.metadata.source.clone(),
                created_at: source.metadata.created_at,
                updated_at: chrono::Utc::now(),
            };
            let dest = Memory::from_validated_with_id(
                source.id,
                dest_name.clone(),
                source.content.clone(),
                metadata,
            );
            dest_repo.save_memory(&dest).await?;
            if let Err(e) = source_repo.delete_memory(source_name, source_scope).await {
                warn!(
                    error = %e,
                    source = %source_name,
                    "cross-repo move: failed to delete source after successful save to destination — data exists in both repos"
                );
                return Err(e);
            }
            Ok(dest)
        }
    }

    // -----------------------------------------------------------------------
    // Read operations — aggregate across all repos
    // -----------------------------------------------------------------------

    /// List memories across all repos, filtered by scope.
    pub async fn list_memories(&self, scope: Option<&Scope>) -> Result<Vec<Memory>, MemoryError> {
        if self.routes.is_empty() {
            return self.default_repo.list_memories(scope).await;
        }

        let mut all_memories = self.default_repo.list_memories(scope).await?;
        for route in &self.routes {
            match route.repo.list_memories(scope).await {
                Ok(memories) => all_memories.extend(memories),
                Err(e) => {
                    warn!(
                        scope = %route.prefix,
                        error = %e,
                        "list_memories: failed to list from scope-specific repo; skipping"
                    );
                }
            }
        }
        Ok(all_memories)
    }

    /// List memories across every repo, failing if any repo cannot be read.
    ///
    /// Rebuilds use this strict form because accepting a partial aggregate as
    /// git truth would silently erase the missing repo from a derived index.
    pub async fn list_memories_strict(&self) -> Result<Vec<Memory>, MemoryError> {
        let mut all_memories = self.default_repo.list_memories(None).await?;
        for route in &self.routes {
            all_memories.extend(route.repo.list_memories(None).await?);
        }
        Ok(all_memories)
    }

    // -----------------------------------------------------------------------
    // Sync operations — push/pull each repo independently
    // -----------------------------------------------------------------------

    /// Sync all repos: pull then push each one.
    ///
    /// Errors on individual repos do not abort the remaining repos — a
    /// network blip on one remote must not block syncing the others. Each
    /// failure is recorded on that repo's [`SyncEntry`] (`pull_error` /
    /// `push_error`) so callers can surface partial failures instead of
    /// reporting a clean sync; check [`MultiSyncResult::all_ok`].
    pub async fn sync_all(
        &self,
        auth: &AuthProvider,
        default_branch: &str,
        pull_first: bool,
    ) -> Result<MultiSyncResult, MemoryError> {
        let mut result = MultiSyncResult::default();

        for (label, repo, branch_override, scope) in self.all_repos() {
            let branch = branch_override.unwrap_or(default_branch);
            let mut entry = SyncEntry {
                label: label.to_string(),
                scope,
                pull: None,
                pull_error: None,
                push_ok: false,
                push_error: None,
                changes: None,
                resolved_changes: None,
                changes_complete: true,
            };

            let mut has_remote = true;

            if pull_first {
                match repo.pull(auth, branch).await {
                    Ok(pull_result) => {
                        if matches!(pull_result, PullResult::NoRemote) {
                            has_remote = false;
                        }
                        // Compute changed memories for incremental reindex.
                        if let PullResult::FastForward {
                            old_head: oh,
                            new_head: nh,
                        }
                        | PullResult::Merged {
                            old_head: oh,
                            new_head: nh,
                            ..
                        } = &pull_result
                        {
                            let repo_clone = Arc::clone(repo);
                            let oh = *oh;
                            let nh = *nh;
                            match crate::repo::traced_spawn_blocking(move || {
                                let changes = repo_clone.diff_changed_memories(oh, nh)?;
                                let resolved = repo_clone.diff_changed_refs(oh, nh)?;
                                Ok::<_, MemoryError>((changes, resolved))
                            })
                            .await
                            {
                                Ok(Ok((changes, resolved))) => {
                                    if !changes.is_empty() {
                                        entry.changes = Some(changes);
                                    }
                                    if !resolved.is_empty() || resolved.unresolved > 0 {
                                        entry.resolved_changes = Some(resolved);
                                    }
                                }
                                Ok(Err(e)) => {
                                    entry.changes_complete = false;
                                    warn!(
                                        label = %label,
                                        error = %e,
                                        "sync: failed to diff changed memories"
                                    );
                                }
                                Err(e) => {
                                    entry.changes_complete = false;
                                    warn!(
                                        label = %label,
                                        error = %e,
                                        "sync: spawn_blocking failed for diff"
                                    );
                                }
                            }
                        }
                        entry.pull = Some(pull_result);
                    }
                    Err(e) => {
                        warn!(
                            label = %label,
                            error = %e,
                            "sync: pull failed — continuing with remaining repos"
                        );
                        entry.pull_error = Some(e.to_string());
                        result.results.push(entry);
                        continue;
                    }
                }
            }

            if has_remote {
                match repo.push(auth, branch).await {
                    Ok(()) => entry.push_ok = true,
                    Err(e) => {
                        warn!(
                            label = %label,
                            error = %e,
                            "sync: push failed — continuing with remaining repos"
                        );
                        entry.push_error = Some(e.to_string());
                    }
                }
            } else {
                entry.push_ok = true;
            }

            result.results.push(entry);
        }

        let failures: Vec<String> = result
            .results
            .iter()
            .filter_map(|e| {
                e.failure_summary()
                    .map(|summary| format!("{}: {summary}", e.label))
            })
            .collect();
        if !failures.is_empty() {
            warn!(
                failed = failures.len(),
                total = result.results.len(),
                "sync: {}/{} repos had errors: {}",
                failures.len(),
                result.results.len(),
                failures.join("; ")
            );
        }

        Ok(result)
    }

    /// Get a composite HEAD SHA covering all repos.
    ///
    /// When no scope-specific routes are configured, returns the default repo's
    /// SHA directly (backward compatible). When routes exist, returns a
    /// deterministic string built from all repos' HEADs so that a change in
    /// *any* repo invalidates the stored index SHA and triggers a reindex.
    pub async fn head_sha(&self) -> Option<String> {
        if self.routes.is_empty() {
            return self.default_repo.head_sha().await;
        }

        // Collect (label, sha) from all repos. Labels are already unique
        // ("default" + each route prefix) and we sort for determinism.
        let mut parts: Vec<(String, String)> = Vec::with_capacity(1 + self.routes.len());

        if let Some(sha) = self.default_repo.head_sha().await {
            parts.push(("default".to_string(), sha));
        }
        for route in &self.routes {
            if let Some(sha) = route.repo.head_sha().await {
                parts.push((route.prefix.clone(), sha));
            }
        }

        if parts.is_empty() {
            return None;
        }

        parts.sort_by(|a, b| a.0.cmp(&b.0));

        let composite = parts
            .iter()
            .map(|(label, sha)| format!("{label}={sha}"))
            .collect::<Vec<_>>()
            .join(";");
        Some(composite)
    }

    /// Return a reference to the repo for a given scope (for direct access when needed).
    pub fn repo(&self, scope: &Scope) -> &Arc<MemoryRepo> {
        self.repo_for_scope(scope)
    }

    /// Check if there are any scope-specific routes configured.
    pub fn has_routes(&self) -> bool {
        !self.routes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        search::{rebuild_lexical_from_router, LexicalIndex},
        types::{MemoryMetadata, MemoryName, ScopeFilter, ScopePath},
    };

    fn test_repo(dir: &tempfile::TempDir) -> Arc<MemoryRepo> {
        Arc::new(MemoryRepo::init_or_open(dir.path(), None).unwrap())
    }

    #[test]
    fn single_routes_everything_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let repo = test_repo(&dir);
        let router = RepoRouter::single(Arc::clone(&repo));

        assert!(Arc::ptr_eq(router.repo_for_scope(&Scope::Root), &repo));
        let sp = ScopePath::new("work").unwrap();
        assert!(Arc::ptr_eq(router.repo_for_scope(&Scope::Path(sp)), &repo));
    }

    #[test]
    fn routes_by_scope_prefix() {
        let default_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let default_repo = test_repo(&default_dir);
        let work_repo = test_repo(&work_dir);

        let router = RepoRouter {
            default_repo: Arc::clone(&default_repo),
            routes: vec![ScopeRoute {
                prefix: "work".to_string(),
                repo: Arc::clone(&work_repo),
                branch: None,
            }],
        };

        // Root goes to default.
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Root),
            &default_repo
        ));

        // "work" goes to work repo.
        let sp = ScopePath::new("work").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &work_repo
        ));

        // "work/subteam" goes to work repo (prefix match).
        let sp = ScopePath::new("work/subteam").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &work_repo
        ));

        // "personal" goes to default.
        let sp = ScopePath::new("personal").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &default_repo
        ));

        // "workflow" does NOT match "work" prefix (segment boundary).
        let sp = ScopePath::new("workflow").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &default_repo
        ));
    }

    #[test]
    fn longest_prefix_wins() {
        let default_dir = tempfile::tempdir().unwrap();
        let org_dir = tempfile::tempdir().unwrap();
        let org_team_dir = tempfile::tempdir().unwrap();
        let default_repo = test_repo(&default_dir);
        let org_repo = test_repo(&org_dir);
        let org_team_repo = test_repo(&org_team_dir);

        let mut routes = vec![
            ScopeRoute {
                prefix: "org".to_string(),
                repo: Arc::clone(&org_repo),
                branch: None,
            },
            ScopeRoute {
                prefix: "org/team".to_string(),
                repo: Arc::clone(&org_team_repo),
                branch: None,
            },
        ];
        routes.sort_by_key(|r| std::cmp::Reverse(r.prefix.len()));

        let router = RepoRouter {
            default_repo: Arc::clone(&default_repo),
            routes,
        };

        // "org/team" matches the longer prefix.
        let sp = ScopePath::new("org/team").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &org_team_repo
        ));

        // "org/other" matches "org".
        let sp = ScopePath::new("org/other").unwrap();
        assert!(Arc::ptr_eq(
            router.repo_for_scope(&Scope::Path(sp)),
            &org_repo
        ));
    }

    #[test]
    fn branch_override() {
        let default_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let default_repo = test_repo(&default_dir);
        let work_repo = test_repo(&work_dir);

        let router = RepoRouter {
            default_repo: Arc::clone(&default_repo),
            routes: vec![ScopeRoute {
                prefix: "work".to_string(),
                repo: Arc::clone(&work_repo),
                branch: Some("develop".to_string()),
            }],
        };

        assert_eq!(router.branch_for_scope(&Scope::Root, "main"), "main");
        let sp = ScopePath::new("work").unwrap();
        assert_eq!(router.branch_for_scope(&Scope::Path(sp), "main"), "develop");
        let sp = ScopePath::new("other").unwrap();
        assert_eq!(router.branch_for_scope(&Scope::Path(sp), "main"), "main");
    }

    #[tokio::test]
    async fn lexical_rebuild_uses_default_and_mapped_repo_truth() {
        let default_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let default_repo = test_repo(&default_dir);
        let work_repo = test_repo(&work_dir);
        let router = RepoRouter {
            default_repo,
            routes: vec![ScopeRoute {
                prefix: "work".to_string(),
                repo: work_repo,
                branch: None,
            }],
        };

        let root = Memory::from_validated(
            MemoryName::new("root-note").unwrap(),
            "rootneedle".to_string(),
            MemoryMetadata::new(Scope::Root, vec![], None),
        );
        let work_scope = Scope::Path(ScopePath::new("work").unwrap());
        let work = Memory::from_validated(
            MemoryName::new("work-note").unwrap(),
            "mappedneedle".to_string(),
            MemoryMetadata::new(work_scope, vec![], None),
        );
        router.save_memory(&root).await.unwrap();
        router.save_memory(&work).await.unwrap();

        let lexical = Arc::new(LexicalIndex::new());
        let count = rebuild_lexical_from_router(&router, &lexical)
            .await
            .unwrap();
        assert_eq!(count, 2);

        let root_hits = lexical.search(&ScopeFilter::All, "rootneedle", 10).unwrap();
        assert_eq!(root_hits[0].0, root.mem_ref().qualified_path());
        let mapped_hits = lexical
            .search(&ScopeFilter::All, "mappedneedle", 10)
            .unwrap();
        assert_eq!(mapped_hits[0].0, work.mem_ref().qualified_path());
    }
}
