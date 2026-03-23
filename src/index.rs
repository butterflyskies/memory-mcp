use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, RwLock},
};

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::{
    error::MemoryError,
    types::{validate_name, Scope, ScopeFilter},
};

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

/// Internal state kept behind the mutex.
struct VectorState {
    index: Index,
    /// Maps usearch u64 keys → memory name strings.
    key_map: HashMap<u64, String>,
    /// Reverse map: memory name strings → usearch u64 keys (derived from key_map).
    name_map: HashMap<String, u64>,
    /// Monotonic counter used to assign unique vector keys.
    next_key: u64,
    /// Commit SHA at the time this index was last saved/loaded.
    commit_sha: Option<String>,
}

/// Wraps `usearch::Index` and a key-map behind a single `std::sync::Mutex`.
///
/// `usearch::Index` is `Send + Sync`, and `HashMap` is `Send`, so
/// `VectorIndex` is `Send + Sync` via the mutex.
pub struct VectorIndex {
    state: Mutex<VectorState>,
}

impl VectorIndex {
    /// Initial capacity reserved when creating a new index.
    const INITIAL_CAPACITY: usize = 1024;

    /// Create a new HNSW index with cosine metric.
    pub fn new(dimensions: usize) -> Result<Self, MemoryError> {
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..Default::default()
        };
        let index =
            Index::new(&options).map_err(|e| MemoryError::Index(format!("create: {}", e)))?;
        // usearch requires reserve() before any add() calls.
        index
            .reserve(Self::INITIAL_CAPACITY)
            .map_err(|e| MemoryError::Index(format!("reserve: {}", e)))?;
        Ok(Self {
            state: Mutex::new(VectorState {
                index,
                key_map: HashMap::new(),
                name_map: HashMap::new(),
                next_key: 0,
                commit_sha: None,
            }),
        })
    }

    /// Grow the index if it doesn't have room for `additional` more vectors.
    ///
    /// Operates on an already-locked `VectorState` reference so callers that
    /// already hold the lock can call this without re-locking.
    fn grow_if_needed_inner(state: &VectorState, additional: usize) -> Result<(), MemoryError> {
        let current_capacity = state.index.capacity();
        let current_size = state.index.size();
        if current_size + additional > current_capacity {
            let new_capacity = (current_capacity + additional).max(current_capacity * 2);
            state
                .index
                .reserve(new_capacity)
                .map_err(|e| MemoryError::Index(format!("reserve: {}", e)))?;
        }
        Ok(())
    }

    /// Ensure the index has capacity for at least `additional` more vectors.
    pub fn grow_if_needed(&self, additional: usize) -> Result<(), MemoryError> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        Self::grow_if_needed_inner(&state, additional)
    }

    /// Atomically increment and return the next unique vector key.
    #[cfg(test)]
    pub fn next_key(&self) -> u64 {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        let key = state.next_key;
        state.next_key += 1;
        key
    }

    /// Find the vector key associated with a qualified memory name.
    pub fn find_key_by_name(&self, name: &str) -> Option<u64> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.name_map.get(name).copied()
    }

    /// Add a vector under the given key, growing the index if necessary.
    #[cfg(test)]
    pub fn add(&self, key: u64, vector: &[f32], name: String) -> Result<(), MemoryError> {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        Self::grow_if_needed_inner(&state, 1)?;
        state
            .index
            .add(key, vector)
            .map_err(|e| MemoryError::Index(format!("add: {}", e)))?;
        state.name_map.insert(name.clone(), key);
        state.key_map.insert(key, name);
        Ok(())
    }

    /// Atomically allocate the next key and add the vector in one lock acquisition.
    /// Returns the assigned key on success. On failure the counter is not advanced.
    pub fn add_with_next_key(&self, vector: &[f32], name: String) -> Result<u64, MemoryError> {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        Self::grow_if_needed_inner(&state, 1)?;
        let key = state.next_key;
        state
            .index
            .add(key, vector)
            .map_err(|e| MemoryError::Index(format!("add: {}", e)))?;
        state.name_map.insert(name.clone(), key);
        state.key_map.insert(key, name);
        state.next_key = state
            .next_key
            .checked_add(1)
            .expect("vector key space exhausted");
        Ok(key)
    }

    /// Search for the `limit` nearest neighbours of `query`.
    ///
    /// Returns `(key, distance)` pairs sorted by ascending distance.
    pub fn search(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(u64, String, f32)>, MemoryError> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        let matches = state
            .index
            .search(query, limit)
            .map_err(|e| MemoryError::Index(format!("search: {}", e)))?;

        let results = matches
            .keys
            .into_iter()
            .zip(matches.distances)
            .filter_map(|(key, dist)| {
                state
                    .key_map
                    .get(&key)
                    .map(|name| (key, name.clone(), dist))
            })
            .collect();
        Ok(results)
    }

    /// Remove a vector by key.
    pub fn remove(&self, key: u64) -> Result<(), MemoryError> {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state
            .index
            .remove(key)
            .map_err(|e| MemoryError::Index(format!("remove: {}", e)))?;
        if let Some(name) = state.key_map.remove(&key) {
            // Only remove from name_map if it still points to this key.
            // An upsert may have already updated name_map to point to a newer key.
            if state.name_map.get(&name).copied() == Some(key) {
                state.name_map.remove(&name);
            }
        }
        Ok(())
    }

    /// Return the commit SHA stored in the index metadata (if any).
    pub fn commit_sha(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.commit_sha.clone()
    }

    /// Set the commit SHA in the index metadata.
    pub fn set_commit_sha(&self, sha: Option<&str>) {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.commit_sha = sha.map(|s| s.to_owned());
    }

    /// Persist the index to `path`. Also writes `<path>.keys.json`.
    ///
    /// If `commit_sha` is `Some`, it is written to the metadata alongside the
    /// key map so the next load can verify freshness.
    pub fn save(&self, path: &Path) -> Result<(), MemoryError> {
        let path_str = path.to_str().ok_or_else(|| MemoryError::InvalidInput {
            reason: "non-UTF-8 index path".to_string(),
        })?;

        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state
            .index
            .save(path_str)
            .map_err(|e| MemoryError::Index(format!("save: {}", e)))?;

        // Persist the key map and counter alongside the index.
        let keys_path = format!("{}.keys.json", path_str);
        let payload = serde_json::json!({
            "key_map": &state.key_map,
            "next_key": state.next_key,
            "commit_sha": state.commit_sha,
        });
        let json = serde_json::to_string(&payload)
            .map_err(|e| MemoryError::Index(format!("keymap serialise: {}", e)))?;
        std::fs::write(&keys_path, json)?;

        Ok(())
    }

    /// Load an existing index from `path`. Also reads `<path>.keys.json`.
    pub fn load(path: &Path) -> Result<Self, MemoryError> {
        let path_str = path.to_str().ok_or_else(|| MemoryError::InvalidInput {
            reason: "non-UTF-8 index path".to_string(),
        })?;

        // We need to know dimensions to create the IndexOptions for load.
        // usearch::Index::load() restores dimensions from the file, so we
        // use placeholder options here — they are overwritten on load.
        let options = IndexOptions {
            dimensions: 1, // overwritten by load()
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..Default::default()
        };
        let index = Index::new(&options)
            .map_err(|e| MemoryError::Index(format!("init for load: {}", e)))?;
        index
            .load(path_str)
            .map_err(|e| MemoryError::Index(format!("load: {}", e)))?;

        // Load the key map and counter.
        let keys_path = format!("{}.keys.json", path_str);
        let (key_map, next_key, commit_sha): (HashMap<u64, String>, u64, Option<String>) =
            if std::path::Path::new(&keys_path).exists() {
                let json = std::fs::read_to_string(&keys_path)?;
                // Support both old format (bare HashMap) and new format ({key_map, next_key}).
                let value: serde_json::Value = serde_json::from_str(&json)
                    .map_err(|e| MemoryError::Index(format!("keymap deserialise: {}", e)))?;
                if value.is_object() && value.get("key_map").is_some() {
                    let km: HashMap<u64, String> = serde_json::from_value(value["key_map"].clone())
                        .map_err(|e| MemoryError::Index(format!("keymap deserialise: {}", e)))?;
                    let nk: u64 = value["next_key"]
                        .as_u64()
                        .unwrap_or_else(|| km.keys().max().map(|k| k + 1).unwrap_or(0));
                    let sha: Option<String> = value
                        .get("commit_sha")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (km, nk, sha)
                } else {
                    // Legacy format: bare HashMap.
                    let km: HashMap<u64, String> = serde_json::from_value(value)
                        .map_err(|e| MemoryError::Index(format!("keymap deserialise: {}", e)))?;
                    let nk = km.keys().max().map(|k| k + 1).unwrap_or(0);
                    (km, nk, None)
                }
            } else {
                (HashMap::new(), 0, None)
            };

        let name_map: HashMap<String, u64> = key_map.iter().map(|(&k, v)| (v.clone(), k)).collect();
        if key_map.len() != name_map.len() {
            tracing::warn!(
                key_map_len = key_map.len(),
                name_map_len = name_map.len(),
                "key_map and name_map have different sizes; index may contain duplicate names"
            );
        }

        Ok(Self {
            state: Mutex::new(VectorState {
                index,
                key_map,
                name_map,
                next_key,
                commit_sha,
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// ScopedIndex
// ---------------------------------------------------------------------------

/// Manages multiple `VectorIndex` instances — one per scope (global, each
/// project) plus a combined "all" index. Every memory exists in exactly two
/// indexes: its scope-specific index + the "all" index.
///
/// `ScopedIndex` is `Send + Sync` because all inner state is protected by
/// `RwLock` / `Mutex`.
pub struct ScopedIndex {
    /// Per-scope indexes (global + each project).
    scopes: RwLock<HashMap<Scope, VectorIndex>>,
    /// Combined index containing all vectors.
    all: VectorIndex,
    /// Embedding dimensions (needed to create new scope indexes).
    dimensions: usize,
}

impl ScopedIndex {
    /// Create a new `ScopedIndex` with empty global + all indexes.
    pub fn new(dimensions: usize) -> Result<Self, MemoryError> {
        let global = VectorIndex::new(dimensions)?;
        let all = VectorIndex::new(dimensions)?;
        let mut scopes = HashMap::new();
        scopes.insert(Scope::Global, global);
        Ok(Self {
            scopes: RwLock::new(scopes),
            all,
            dimensions,
        })
    }

    /// Ensure the scope's index exists, creating it if missing.
    pub fn ensure_scope(&self, scope: &Scope) -> Result<(), MemoryError> {
        // Fast path: check with a read lock first.
        {
            let scopes = self.scopes.read().expect("scopes lock poisoned");
            if scopes.contains_key(scope) {
                return Ok(());
            }
        }
        // Slow path: acquire write lock and create.
        let mut scopes = self.scopes.write().expect("scopes lock poisoned");
        // Re-check after acquiring write lock (another thread may have inserted).
        if !scopes.contains_key(scope) {
            scopes.insert(scope.clone(), VectorIndex::new(self.dimensions)?);
        }
        Ok(())
    }

    /// Insert `vector` into both the scope-specific index and the all-index.
    ///
    /// Handles upserts: if `qualified_name` already exists in either index, the
    /// old entry is removed after the new one is successfully inserted.
    ///
    /// Returns the key assigned in the all-index.
    pub fn add(
        &self,
        scope: &Scope,
        vector: &[f32],
        qualified_name: String,
    ) -> Result<u64, MemoryError> {
        // Write lock serialises the full find→insert→remove composite so
        // concurrent upserts for the same name cannot interleave. Reads
        // (via `search`) use a read lock and are not blocked by other reads.
        let mut scopes = self.scopes.write().expect("scopes lock poisoned");

        // Ensure scope index exists (inline, since we already hold write lock).
        if !scopes.contains_key(scope) {
            scopes.insert(scope.clone(), VectorIndex::new(self.dimensions)?);
        }

        let scope_idx = scopes
            .get(scope)
            .expect("scope index must exist after insert");

        // Capture old keys before inserting new ones.
        let old_scope_key = scope_idx.find_key_by_name(&qualified_name);
        let old_all_key = self.all.find_key_by_name(&qualified_name);

        // Insert into scope index first.
        let new_scope_key = scope_idx.add_with_next_key(vector, qualified_name.clone())?;

        // Insert into all-index; if this fails, roll back scope insert.
        let all_key = match self.all.add_with_next_key(vector, qualified_name) {
            Ok(key) => key,
            Err(e) => {
                let _ = scope_idx.remove(new_scope_key);
                return Err(e);
            }
        };

        // Both succeeded — now clean up old entries.
        if let Some(key) = old_scope_key {
            let _ = scope_idx.remove(key);
        }
        if let Some(key) = old_all_key {
            let _ = self.all.remove(key);
        }

        Ok(all_key)
    }

    /// Remove a memory by qualified name from both the scope-specific index
    /// and the all-index.
    ///
    /// Both removals are best-effort: an error in one does not prevent the
    /// other from running. Returns `Ok(())` regardless of individual failures.
    pub fn remove(&self, scope: &Scope, qualified_name: &str) -> Result<(), MemoryError> {
        // Write lock serialises with concurrent adds for the same name.
        let scopes = self.scopes.write().expect("scopes lock poisoned");

        // Remove from scope index (best-effort).
        if let Some(scope_idx) = scopes.get(scope) {
            if let Some(key) = scope_idx.find_key_by_name(qualified_name) {
                if let Err(e) = scope_idx.remove(key) {
                    tracing::warn!(
                        qualified_name = %qualified_name,
                        error = %e,
                        "scope index removal failed; continuing to all-index"
                    );
                }
            }
        }

        // Remove from all-index (best-effort).
        if let Some(key) = self.all.find_key_by_name(qualified_name) {
            if let Err(e) = self.all.remove(key) {
                tracing::warn!(
                    qualified_name = %qualified_name,
                    error = %e,
                    "all-index removal failed"
                );
            }
        }

        Ok(())
    }

    /// Search for the nearest neighbours of `query`, routing to the correct
    /// indexes based on `filter`.
    ///
    /// | `filter`               | Indexes searched          | Merge strategy             |
    /// |------------------------|---------------------------|----------------------------|
    /// | `GlobalOnly`           | `global`                  | Direct top-k               |
    /// | `ProjectAndGlobal(p)`  | `global` + `projects/p`   | Merge by distance, top-k   |
    /// | `All`                  | `all` combined index       | Direct top-k               |
    pub fn search(
        &self,
        filter: &ScopeFilter,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(u64, String, f32)>, MemoryError> {
        match filter {
            ScopeFilter::All => self.all.search(query, limit),

            ScopeFilter::GlobalOnly => {
                let scopes = self.scopes.read().expect("scopes lock poisoned");
                match scopes.get(&Scope::Global) {
                    Some(global_idx) => global_idx.search(query, limit),
                    None => Ok(Vec::new()),
                }
            }

            ScopeFilter::ProjectAndGlobal(project_name) => {
                let scopes = self.scopes.read().expect("scopes lock poisoned");
                let project_scope = Scope::Project(project_name.clone());

                let mut combined: Vec<(u64, String, f32)> = Vec::new();

                if let Some(global_idx) = scopes.get(&Scope::Global) {
                    let mut global_results = global_idx.search(query, limit)?;
                    combined.append(&mut global_results);
                }

                if let Some(proj_idx) = scopes.get(&project_scope) {
                    let mut proj_results = proj_idx.search(query, limit)?;
                    combined.append(&mut proj_results);
                }

                // Deduplicate by qualified name (HashSet ensures non-adjacent dupes are caught).
                let mut seen = std::collections::HashSet::new();
                combined.retain(|(_, name, _)| seen.insert(name.clone()));
                // Sort by ascending distance and take top-k.
                combined.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
                combined.truncate(limit);
                Ok(combined)
            }
        }
    }

    /// Find the key for a given qualified name in the **all-index** (not scope-specific).
    ///
    /// This is the canonical lookup — the all-index contains every memory regardless of scope.
    pub fn find_key_by_name(&self, qualified_name: &str) -> Option<u64> {
        self.all.find_key_by_name(qualified_name)
    }

    /// Grow all indexes to accommodate `additional` more vectors.
    ///
    /// Reserved for future batch-insert operations; no production callers currently exist.
    #[allow(dead_code)]
    pub fn grow_if_needed(&self, additional: usize) -> Result<(), MemoryError> {
        self.all.grow_if_needed(additional)?;
        let scopes = self.scopes.read().expect("scopes lock poisoned");
        for idx in scopes.values() {
            idx.grow_if_needed(additional)?;
        }
        Ok(())
    }

    /// Persist all indexes to subdirectories under `dir`.
    ///
    /// Layout:
    /// ```text
    /// dir/
    ///   all/index.usearch  (+ .keys.json)
    ///   global/index.usearch
    ///   projects/foo/index.usearch
    /// ```
    pub fn save(&self, dir: &Path) -> Result<(), MemoryError> {
        std::fs::create_dir_all(dir)?;

        // Write a dirty marker — if we crash mid-save, the next load will see
        // this and ignore commit SHAs (forcing a fresh rebuild).
        let marker = dir.join(".save-in-progress");
        std::fs::write(&marker, b"")?;

        // Persist all-index.
        let all_dir = dir.join("all");
        std::fs::create_dir_all(&all_dir)?;
        self.all.save(&all_dir.join("index.usearch"))?;

        // Persist per-scope indexes.
        let scopes = self.scopes.read().expect("scopes lock poisoned");
        for (scope, idx) in scopes.iter() {
            let scope_dir = dir.join(scope.dir_prefix());
            std::fs::create_dir_all(&scope_dir)?;
            idx.save(&scope_dir.join("index.usearch"))?;
        }

        // Remove marker — save completed successfully.
        let _ = std::fs::remove_file(&marker);

        Ok(())
    }

    /// Load all indexes from subdirectories under `dir`.
    ///
    /// Missing subdirectories are treated as empty — those scopes will be
    /// rebuilt incrementally on next use.
    pub fn load(dir: &Path, dimensions: usize) -> Result<Self, MemoryError> {
        // If a previous save was interrupted, the on-disk state may be
        // inconsistent (some indexes from current state, others from prior).
        // Rather than loading mixed data, start fresh — indexes are a cache
        // that can always be rebuilt from the source-of-truth markdown files.
        let dirty_marker = dir.join(".save-in-progress");
        if dirty_marker.exists() {
            tracing::warn!("detected interrupted index save — discarding indexes");
            let _ = std::fs::remove_file(&dirty_marker);
            return Self::new(dimensions);
        }

        // Load all-index.
        let all_path = dir.join("all").join("index.usearch");
        let all = if all_path.exists() {
            VectorIndex::load(&all_path)?
        } else {
            VectorIndex::new(dimensions)?
        };

        let mut scopes: HashMap<Scope, VectorIndex> = HashMap::new();

        // Load global index.
        let global_path = dir.join("global").join("index.usearch");
        let global = if global_path.exists() {
            VectorIndex::load(&global_path)?
        } else {
            VectorIndex::new(dimensions)?
        };
        scopes.insert(Scope::Global, global);

        // Scan for project indexes under projects/*/
        let projects_dir = dir.join("projects");
        if projects_dir.is_dir() {
            let entries = std::fs::read_dir(&projects_dir)
                .map_err(|e| MemoryError::Index(format!("read projects dir: {}", e)))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| MemoryError::Index(format!("read dir entry: {}", e)))?;
                let path = entry.path();
                if path.is_dir() {
                    let project_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| {
                            MemoryError::Index("non-UTF-8 project directory name".to_string())
                        })?;
                    if let Err(e) = validate_name(&project_name) {
                        tracing::warn!(
                            project_name = %project_name,
                            error = %e,
                            "skipping project index with invalid name"
                        );
                        continue;
                    }
                    let index_path = path.join("index.usearch");
                    if index_path.exists() {
                        let idx = VectorIndex::load(&index_path)?;
                        scopes.insert(Scope::Project(project_name), idx);
                    }
                }
            }
        }

        Ok(Self {
            scopes: RwLock::new(scopes),
            all,
            dimensions,
        })
    }

    /// Read the commit SHA from the all-index metadata.
    pub fn commit_sha(&self) -> Option<String> {
        self.all.commit_sha()
    }

    /// Set the commit SHA on all sub-indexes.
    pub fn set_commit_sha(&self, sha: Option<&str>) {
        self.all.set_commit_sha(sha);
        let scopes = self.scopes.read().expect("scopes lock poisoned");
        for idx in scopes.values() {
            idx.set_commit_sha(sha);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index() -> VectorIndex {
        VectorIndex::new(4).expect("failed to create index")
    }

    fn dummy_vec() -> Vec<f32> {
        vec![1.0, 0.0, 0.0, 0.0]
    }

    /// Verify that `remove(old_key)` does NOT clobber `name_map` when an
    /// upsert has already updated `name_map` to point to a newer key.
    ///
    /// Pattern: add_with_next_key("name") → old_key
    ///          add_with_next_key("name") → new_key  (name_map now points to new_key)
    ///          remove(old_key)
    ///          find_key_by_name("name") must return new_key (not None)
    #[test]
    fn remove_old_key_does_not_clobber_upserted_name_map_entry() {
        let index = make_index();
        let v = dummy_vec();

        // First insert — establishes old_key.
        let old_key = index
            .add_with_next_key(&v, "global/foo".to_string())
            .expect("first add failed");

        // Upsert (second insert for same name) — name_map now points to new_key.
        let new_key = index
            .add_with_next_key(&v, "global/foo".to_string())
            .expect("second add failed");

        assert_ne!(old_key, new_key, "keys must differ");

        // Remove the OLD key — should not disturb name_map's entry for new_key.
        index.remove(old_key).expect("remove failed");

        // name_map must still resolve "global/foo" to new_key.
        assert_eq!(
            index.find_key_by_name("global/foo"),
            Some(new_key),
            "name_map entry for new_key was incorrectly removed"
        );
    }

    /// Removing the current (only) key should clear the name_map entry.
    #[test]
    fn remove_only_key_clears_name_map() {
        let index = make_index();
        let v = dummy_vec();

        let key = index
            .add_with_next_key(&v, "global/bar".to_string())
            .expect("add failed");

        index.remove(key).expect("remove failed");

        assert_eq!(
            index.find_key_by_name("global/bar"),
            None,
            "name_map entry should have been cleared"
        );
    }

    // -----------------------------------------------------------------------
    // ScopedIndex tests
    // -----------------------------------------------------------------------

    fn make_scoped() -> ScopedIndex {
        ScopedIndex::new(8).expect("failed to create scoped index")
    }

    fn vec_a() -> Vec<f32> {
        vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    }

    fn vec_b() -> Vec<f32> {
        vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    }

    fn vec_c() -> Vec<f32> {
        vec![0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    }

    #[test]
    fn scoped_index_add_inserts_into_scope_and_all() {
        let si = make_scoped();
        let scope = Scope::Global;
        let name = "global/memory-a".to_string();

        si.add(&scope, &vec_a(), name.clone()).expect("add failed");

        // Should be findable in the all-index via find_key_by_name.
        assert!(
            si.find_key_by_name(&name).is_some(),
            "should be in all-index"
        );

        // Should also be in scope-specific index — verify via search.
        let results = si
            .search(&ScopeFilter::GlobalOnly, &vec_a(), 5)
            .expect("search failed");
        assert!(
            results.iter().any(|(_, n, _)| n == &name),
            "should be found in global search"
        );
    }

    #[test]
    fn scoped_index_remove_removes_from_both() {
        let si = make_scoped();
        let scope = Scope::Global;
        let name = "global/memory-rm".to_string();

        si.add(&scope, &vec_a(), name.clone()).expect("add failed");
        assert!(si.find_key_by_name(&name).is_some(), "should exist");

        si.remove(&scope, &name).expect("remove failed");

        assert!(
            si.find_key_by_name(&name).is_none(),
            "should be gone from all-index"
        );

        let results = si
            .search(&ScopeFilter::GlobalOnly, &vec_a(), 5)
            .expect("search failed");
        assert!(
            !results.iter().any(|(_, n, _)| n == &name),
            "should not appear in global search after removal"
        );
    }

    #[test]
    fn scoped_index_search_global_only() {
        let si = make_scoped();
        let proj = Scope::Project("myproj".to_string());

        si.add(&Scope::Global, &vec_a(), "global/mem-global".to_string())
            .expect("add global failed");
        si.add(&proj, &vec_b(), "projects/myproj/mem-proj".to_string())
            .expect("add project failed");

        let results = si
            .search(&ScopeFilter::GlobalOnly, &vec_a(), 5)
            .expect("search failed");

        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"global/mem-global"),
            "should contain global"
        );
        assert!(
            !names.contains(&"projects/myproj/mem-proj"),
            "should NOT contain project memory"
        );
    }

    #[test]
    fn scoped_index_search_project_and_global() {
        let si = make_scoped();
        let proj_a = Scope::Project("alpha".to_string());
        let proj_b = Scope::Project("beta".to_string());

        si.add(&Scope::Global, &vec_a(), "global/g1".to_string())
            .expect("add global failed");
        si.add(&proj_a, &vec_b(), "projects/alpha/a1".to_string())
            .expect("add alpha failed");
        si.add(&proj_b, &vec_c(), "projects/beta/b1".to_string())
            .expect("add beta failed");

        let results = si
            .search(
                &ScopeFilter::ProjectAndGlobal("alpha".to_string()),
                &vec_a(),
                10,
            )
            .expect("search failed");

        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"global/g1"), "should contain global");
        assert!(names.contains(&"projects/alpha/a1"), "should contain alpha");
        assert!(
            !names.contains(&"projects/beta/b1"),
            "should NOT contain beta"
        );
    }

    #[test]
    fn scoped_index_search_all() {
        let si = make_scoped();
        let proj = Scope::Project("foo".to_string());

        si.add(&Scope::Global, &vec_a(), "global/x".to_string())
            .expect("add global");
        si.add(&proj, &vec_b(), "projects/foo/y".to_string())
            .expect("add project");

        let results = si
            .search(&ScopeFilter::All, &vec_a(), 10)
            .expect("search failed");

        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"global/x"), "all should include global");
        assert!(
            names.contains(&"projects/foo/y"),
            "all should include project"
        );
    }

    #[test]
    fn scoped_index_upsert_replaces_old_entry() {
        let si = make_scoped();
        let name = "global/memo".to_string();
        si.add(&Scope::Global, &vec_a(), name.clone()).unwrap();
        si.add(&Scope::Global, &vec_b(), name.clone()).unwrap();
        // Should have exactly one entry in all-index search.
        let results = si.search(&ScopeFilter::All, &vec_b(), 10).unwrap();
        assert_eq!(
            results.iter().filter(|(_, n, _)| n == &name).count(),
            1,
            "upsert should leave exactly one entry for the name"
        );
    }

    #[test]
    fn scoped_index_dirty_marker_discards_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let si = ScopedIndex::new(8).expect("create");
        si.add(&Scope::Global, &vec_a(), "global/test-mem".to_string())
            .expect("add");
        si.set_commit_sha(Some("abc123"));
        si.save(dir.path()).expect("save");

        // Simulate interrupted save by re-creating the marker.
        std::fs::write(dir.path().join(".save-in-progress"), b"").unwrap();

        // Load should discard all indexes and return fresh empty ones.
        let loaded = ScopedIndex::load(dir.path(), 8).expect("load");
        assert!(
            loaded.commit_sha().is_none(),
            "dirty marker should result in no SHA"
        );
        assert!(
            loaded.find_key_by_name("global/test-mem").is_none(),
            "dirty marker should discard all indexed data"
        );
        assert!(
            !dir.path().join(".save-in-progress").exists(),
            "marker should be cleaned up"
        );
    }

    #[test]
    fn scoped_index_save_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let si = ScopedIndex::new(8).expect("create");
        let proj = Scope::Project("rtrip".to_string());

        si.add(&Scope::Global, &vec_a(), "global/rt-global".to_string())
            .expect("add global");
        si.add(&proj, &vec_b(), "projects/rtrip/rt-proj".to_string())
            .expect("add project");

        si.save(dir.path()).expect("save failed");

        let loaded = ScopedIndex::load(dir.path(), 8).expect("load failed");

        // Verify all-index finds both memories.
        assert!(
            loaded.find_key_by_name("global/rt-global").is_some(),
            "global memory should survive round-trip"
        );
        assert!(
            loaded.find_key_by_name("projects/rtrip/rt-proj").is_some(),
            "project memory should survive round-trip"
        );

        // Verify search still works after reload.
        let results = loaded
            .search(
                &ScopeFilter::ProjectAndGlobal("rtrip".to_string()),
                &vec_a(),
                10,
            )
            .expect("search failed");
        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"global/rt-global"));
        assert!(names.contains(&"projects/rtrip/rt-proj"));
    }

    #[test]
    fn scoped_index_same_short_name_different_scopes_coexist() {
        let si = make_scoped();
        si.add(&Scope::Global, &vec_a(), "global/foo".to_string())
            .unwrap();
        si.add(
            &Scope::Project("p".into()),
            &vec_b(),
            "projects/p/foo".to_string(),
        )
        .unwrap();
        assert!(si.find_key_by_name("global/foo").is_some());
        assert!(si.find_key_by_name("projects/p/foo").is_some());
        assert_ne!(
            si.find_key_by_name("global/foo"),
            si.find_key_by_name("projects/p/foo"),
            "different scopes should have distinct keys"
        );
    }
}
