use std::{collections::HashMap, path::Path, sync::Mutex};

use crate::{
    error::MemoryError,
    index::VectorStore,
    types::{Scope, ScopeFilter},
};

// ---------------------------------------------------------------------------
// InMemoryState
// ---------------------------------------------------------------------------

struct InMemoryState {
    /// All entries keyed by qualified name, storing (scope, vector).
    entries: HashMap<String, (Scope, Vec<f32>)>,
    /// Monotonic key counter.
    key_counter: u64,
    /// Maps qualified name → assigned key.
    key_map: HashMap<String, u64>,
    /// Stored commit SHA.
    commit_sha: Option<String>,
}

// ---------------------------------------------------------------------------
// InMemoryStore
// ---------------------------------------------------------------------------

/// HashMap-based `VectorStore` implementation for tests.
///
/// Performs brute-force cosine similarity search — no HNSW required.
/// `save` and `load` are no-ops. `is_ready` is configurable.
#[non_exhaustive]
pub struct InMemoryStore {
    state: Mutex<InMemoryState>,
    dimensions: usize,
    ready: bool,
}

impl InMemoryStore {
    /// Create a new `InMemoryStore` with the given embedding dimensionality.
    ///
    /// `is_ready` defaults to `true`. Use [`InMemoryStore::with_ready`] to
    /// override.
    pub fn new(dimensions: usize) -> Self {
        Self {
            state: Mutex::new(InMemoryState {
                entries: HashMap::new(),
                key_counter: 0,
                key_map: HashMap::new(),
                commit_sha: None,
            }),
            dimensions,
            ready: true,
        }
    }

    /// Override the value returned by `is_ready()`.
    pub fn with_ready(mut self, ready: bool) -> Self {
        self.ready = ready;
        self
    }
}

/// Compute cosine similarity between two equal-length vectors.
///
/// Returns `0.0` if either vector is zero-magnitude.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
    }
}

/// Convert cosine similarity to a distance metric (lower = more similar),
/// matching the usearch convention.
fn similarity_to_distance(sim: f32) -> f32 {
    1.0 - sim
}

impl crate::index::sealed::Sealed for InMemoryStore {}

impl VectorStore for InMemoryStore {
    // Scope filtering relies on qualified_name encoding the scope (e.g.
    // "global:foo" or "project:bar:baz"), which UsearchStoreInner guarantees.
    // The scope parameter is stored alongside the entry for search filtering
    // but is not used as a separate index key.
    fn add(
        &self,
        scope: &Scope,
        vector: &[f32],
        qualified_name: String,
    ) -> Result<u64, MemoryError> {
        if vector.len() != self.dimensions {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "expected {} dimensions, got {}",
                    self.dimensions,
                    vector.len()
                ),
            });
        }
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        let key = state.key_counter;
        state.key_counter = state
            .key_counter
            .checked_add(1)
            .expect("key space exhausted");
        state
            .entries
            .insert(qualified_name.clone(), (scope.clone(), vector.to_vec()));
        state.key_map.insert(qualified_name, key);
        Ok(key)
    }

    fn remove(&self, _scope: &Scope, qualified_name: &str) -> Result<(), MemoryError> {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.entries.remove(qualified_name);
        state.key_map.remove(qualified_name);
        Ok(())
    }

    fn search(
        &self,
        filter: &ScopeFilter,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(u64, String, f32)>, MemoryError> {
        if query.len() != self.dimensions {
            return Err(MemoryError::InvalidInput {
                reason: format!(
                    "expected {} dimensions, got {}",
                    self.dimensions,
                    query.len()
                ),
            });
        }
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");

        let mut candidates: Vec<(u64, String, f32)> = state
            .entries
            .iter()
            .filter(|(_, (scope, _))| scope_matches(filter, scope))
            .map(|(name, (_, vec))| {
                let key = state
                    .key_map
                    .get(name)
                    .copied()
                    .expect("invariant: every entry has a key_map entry");
                let sim = cosine_similarity(query, vec);
                let dist = similarity_to_distance(sim);
                (key, name.clone(), dist)
            })
            .collect();

        // Sort by ascending distance (lower distance = more similar).
        candidates.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(limit);
        Ok(candidates)
    }

    fn find_by_name(&self, qualified_name: &str) -> Option<u64> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.key_map.get(qualified_name).copied()
    }

    fn save(&self, _dir: &Path) -> Result<(), MemoryError> {
        // No-op: InMemoryStore is an in-memory test double.
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn commit_sha(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.commit_sha.clone()
    }

    fn set_commit_sha(&self, sha: Option<&str>) {
        let mut state = self
            .state
            .lock()
            .expect("lock poisoned — prior panic corrupted state");
        state.commit_sha = sha.map(|s| s.to_owned());
    }
}

/// Returns `true` if `scope` should be included given `filter`.
fn scope_matches(filter: &ScopeFilter, scope: &Scope) -> bool {
    match filter {
        ScopeFilter::All => true,
        ScopeFilter::GlobalOnly => matches!(scope, Scope::Global),
        ScopeFilter::ProjectAndGlobal(project_name) => match scope {
            Scope::Global => true,
            Scope::Project(p) => p == project_name,
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VectorStore;

    fn make_store() -> InMemoryStore {
        InMemoryStore::new(4)
    }

    fn vec_a() -> Vec<f32> {
        vec![1.0, 0.0, 0.0, 0.0]
    }

    fn vec_b() -> Vec<f32> {
        vec![0.0, 1.0, 0.0, 0.0]
    }

    fn vec_c() -> Vec<f32> {
        vec![0.0, 0.0, 1.0, 0.0]
    }

    // TC-02a: add + find_by_name
    #[test]
    fn tc02a_add_and_find_by_name() {
        let store: &dyn VectorStore = &make_store();
        store
            .add(&Scope::Global, &vec_a(), "global/mem1".to_string())
            .expect("add failed");
        assert!(
            store.find_by_name("global/mem1").is_some(),
            "TC-02a: find_by_name should return Some after add"
        );
    }

    // TC-02b: remove clears entry
    #[test]
    fn tc02b_remove_clears_entry() {
        let store: &dyn VectorStore = &make_store();
        store
            .add(&Scope::Global, &vec_a(), "global/mem2".to_string())
            .expect("add failed");
        store
            .remove(&Scope::Global, "global/mem2")
            .expect("remove failed");
        assert!(
            store.find_by_name("global/mem2").is_none(),
            "TC-02b: find_by_name should return None after remove"
        );
    }

    // TC-02c: search with GlobalOnly filter
    #[test]
    fn tc02c_search_global_only() {
        let store: &dyn VectorStore = &make_store();
        let proj = Scope::Project("p".to_string());

        store
            .add(&Scope::Global, &vec_a(), "global/g1".to_string())
            .expect("add global");
        store
            .add(&proj, &vec_b(), "projects/p/p1".to_string())
            .expect("add project");

        let results = store
            .search(&ScopeFilter::GlobalOnly, &vec_a(), 10)
            .expect("search failed");
        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"global/g1"), "should contain global");
        assert!(
            !names.contains(&"projects/p/p1"),
            "should NOT contain project"
        );
    }

    // TC-02d: search with ProjectAndGlobal filter
    #[test]
    fn tc02d_search_project_and_global() {
        let store: &dyn VectorStore = &make_store();
        let proj_a = Scope::Project("alpha".to_string());
        let proj_b = Scope::Project("beta".to_string());

        store
            .add(&Scope::Global, &vec_a(), "global/g1".to_string())
            .expect("add global");
        store
            .add(&proj_a, &vec_b(), "projects/alpha/a1".to_string())
            .expect("add alpha");
        store
            .add(&proj_b, &vec_c(), "projects/beta/b1".to_string())
            .expect("add beta");

        let results = store
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

    // TC-02e: search with All filter
    #[test]
    fn tc02e_search_all() {
        let store: &dyn VectorStore = &make_store();
        let proj = Scope::Project("foo".to_string());

        store
            .add(&Scope::Global, &vec_a(), "global/x".to_string())
            .expect("add global");
        store
            .add(&proj, &vec_b(), "projects/foo/y".to_string())
            .expect("add project");

        let results = store
            .search(&ScopeFilter::All, &vec_a(), 10)
            .expect("search failed");
        let names: Vec<&str> = results.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"global/x"), "all should include global");
        assert!(
            names.contains(&"projects/foo/y"),
            "all should include project"
        );
    }

    // TC-05c: InMemoryStore returns same MemoryError variants
    // InMemoryStore's add/remove/search don't return errors in normal operation,
    // but verify the Ok path works and returns appropriate types.
    #[test]
    fn tc05c_in_memory_store_returns_ok_variants() {
        let store: &dyn VectorStore = &make_store();
        let result = store.add(&Scope::Global, &vec_a(), "global/tc05c".to_string());
        assert!(
            result.is_ok(),
            "TC-05c: add should return Ok, got: {:?}",
            result
        );
        let result = store.search(&ScopeFilter::All, &vec_a(), 5);
        assert!(result.is_ok(), "TC-05c: search should return Ok");
        let result = store.remove(&Scope::Global, "global/tc05c");
        assert!(result.is_ok(), "TC-05c: remove should return Ok");
    }

    // TC-06b: InMemoryStore::is_ready() returns configured value
    #[test]
    fn tc06b_in_memory_store_is_ready_default_true() {
        let store = InMemoryStore::new(4);
        assert!(
            store.is_ready(),
            "TC-06b: InMemoryStore::is_ready() should return true by default"
        );
    }

    #[test]
    fn tc06b_in_memory_store_is_ready_configured_false() {
        let store = InMemoryStore::new(4).with_ready(false);
        assert!(
            !store.is_ready(),
            "TC-06b: InMemoryStore::is_ready() should return false when configured so"
        );
    }

    #[test]
    fn in_memory_store_dimensions() {
        let store = InMemoryStore::new(128);
        assert_eq!(store.dimensions(), 128);
    }

    #[test]
    fn in_memory_store_commit_sha_round_trip() {
        let store: &dyn VectorStore = &InMemoryStore::new(4);
        assert!(store.commit_sha().is_none());
        store.set_commit_sha(Some("abc123"));
        assert_eq!(store.commit_sha(), Some("abc123".to_string()));
        store.set_commit_sha(None);
        assert!(store.commit_sha().is_none());
    }

    #[test]
    fn in_memory_store_save_is_noop() {
        let store: &dyn VectorStore = &make_store();
        let dir = tempfile::tempdir().expect("tempdir");
        store
            .add(&Scope::Global, &vec_a(), "global/save-test".to_string())
            .expect("add");
        let result = store.save(dir.path());
        assert!(result.is_ok(), "save should be a no-op Ok");
    }

    #[test]
    fn in_memory_store_search_results_sorted_by_distance() {
        let store: &dyn VectorStore = &make_store();
        // vec_a is [1,0,0,0]; searching for [1,0,0,0] should rank it first.
        store
            .add(&Scope::Global, &vec_a(), "global/closest".to_string())
            .expect("add a");
        store
            .add(&Scope::Global, &vec_b(), "global/farther".to_string())
            .expect("add b");

        let results = store
            .search(&ScopeFilter::All, &vec_a(), 10)
            .expect("search");
        assert_eq!(results.len(), 2);
        // Distance to vec_a from query vec_a should be 0 (cosine distance = 1 - 1 = 0).
        assert!(
            results[0].2 <= results[1].2,
            "results should be sorted by ascending distance"
        );
        assert_eq!(results[0].1, "global/closest");
    }

    #[test]
    fn tc05c_in_memory_store_dimension_mismatch_returns_invalid_input() {
        let store = InMemoryStore::new(4);
        let wrong_dims = vec![1.0_f32, 0.0]; // 2 dims, store expects 4
        let err = store
            .add(&Scope::Global, &wrong_dims, "global/bad-dims".to_string())
            .unwrap_err();
        assert!(
            matches!(err, MemoryError::InvalidInput { .. }),
            "TC-05c: dimension mismatch should return InvalidInput, got: {:?}",
            err
        );
    }

    #[test]
    fn in_memory_store_upsert_overwrites() {
        let store: &dyn VectorStore = &make_store();
        let name = "global/upsert-me".to_string();
        let key1 = store
            .add(&Scope::Global, &vec_a(), name.clone())
            .expect("first add");
        let key2 = store
            .add(&Scope::Global, &vec_b(), name.clone())
            .expect("second add");
        // Keys should differ (monotonic counter).
        assert_ne!(key1, key2);
        // The latest key wins in find_by_name.
        assert_eq!(store.find_by_name(&name), Some(key2));
    }
}
