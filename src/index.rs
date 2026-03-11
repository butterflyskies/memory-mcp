use std::{collections::HashMap, path::Path, sync::Mutex};

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::MemoryError;

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

/// Internal state kept behind the mutex.
struct VectorState {
    index: Index,
    /// Maps usearch u64 keys → memory name strings.
    key_map: HashMap<u64, String>,
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

    /// Add a vector under the given key, growing the index if necessary.
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
        state.key_map.insert(key, name);
        Ok(())
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
        state.key_map.remove(&key);
        Ok(())
    }

    /// Persist the index to `path`. Also writes `<path>.keys.json`.
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

        // Persist the key map alongside the index.
        let keys_path = format!("{}.keys.json", path_str);
        let json = serde_json::to_string(&state.key_map)
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

        // Load the key map.
        let keys_path = format!("{}.keys.json", path_str);
        let key_map: HashMap<u64, String> = if std::path::Path::new(&keys_path).exists() {
            let json = std::fs::read_to_string(&keys_path)?;
            serde_json::from_str(&json)
                .map_err(|e| MemoryError::Index(format!("keymap deserialise: {}", e)))?
        } else {
            HashMap::new()
        };

        Ok(Self {
            state: Mutex::new(VectorState { index, key_map }),
        })
    }
}
