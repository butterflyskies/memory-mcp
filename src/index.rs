use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::error::MemoryError;

// ---------------------------------------------------------------------------
// KeyMap — maps usearch u64 keys → memory name strings
// ---------------------------------------------------------------------------

/// A thread-safe map from vector-index keys to memory names.
///
/// usearch stores vectors under opaque `u64` keys. `KeyMap` lets us
/// round-trip from a search result back to the memory name.
#[derive(Clone, Default)]
pub struct KeyMap {
    inner: Arc<RwLock<HashMap<u64, String>>>,
}

impl KeyMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: u64, name: String) {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, name);
    }

    pub fn remove(&self, key: u64) {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    pub fn get(&self, key: u64) -> Option<String> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
    }

    /// Serialise to JSON for persistence alongside the index.
    pub fn to_json(&self) -> Result<String, MemoryError> {
        let map = self.inner.read().unwrap_or_else(|e| e.into_inner());
        serde_json::to_string(&*map)
            .map_err(|e| MemoryError::Index(format!("keymap serialise: {}", e)))
    }

    /// Deserialise from JSON.
    pub fn from_json(json: &str) -> Result<Self, MemoryError> {
        let map: HashMap<u64, String> = serde_json::from_str(json)
            .map_err(|e| MemoryError::Index(format!("keymap deserialise: {}", e)))?;
        Ok(Self {
            inner: Arc::new(RwLock::new(map)),
        })
    }
}

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

/// Wraps `usearch::Index` and provides async-safe methods.
///
/// `usearch::Index` is `Send + Sync`, so we can share it behind an `Arc`
/// without any additional mutex.
pub struct VectorIndex {
    inner: Arc<Index>,
    pub key_map: KeyMap,
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
            inner: Arc::new(index),
            key_map: KeyMap::new(),
        })
    }

    /// Ensure the index has capacity for at least `additional` more vectors.
    pub fn grow_if_needed(&self, additional: usize) -> Result<(), MemoryError> {
        let current_capacity = self.inner.capacity();
        let current_size = self.inner.size();
        if current_size + additional > current_capacity {
            let new_capacity = (current_capacity + additional).max(current_capacity * 2);
            self.inner
                .reserve(new_capacity)
                .map_err(|e| MemoryError::Index(format!("reserve: {}", e)))?;
        }
        Ok(())
    }

    /// Add a vector under the given key.
    pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), MemoryError> {
        self.inner
            .add(key, vector)
            .map_err(|e| MemoryError::Index(format!("add: {}", e)))
    }

    /// Search for the `limit` nearest neighbours of `query`.
    ///
    /// Returns `(key, distance)` pairs sorted by ascending distance.
    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<(u64, f32)>, MemoryError> {
        let matches = self
            .inner
            .search(query, limit)
            .map_err(|e| MemoryError::Index(format!("search: {}", e)))?;

        Ok(matches.keys.into_iter().zip(matches.distances).collect())
    }

    /// Remove a vector by key.
    pub fn remove(&self, key: u64) -> Result<(), MemoryError> {
        self.inner
            .remove(key)
            .map_err(|e| MemoryError::Index(format!("remove: {}", e)))?;
        Ok(())
    }

    /// Persist the index to `path`. Also writes `<path>.keys.json`.
    pub fn save(&self, path: &Path) -> Result<(), MemoryError> {
        let path_str = path.to_str().ok_or_else(|| MemoryError::InvalidInput {
            reason: "non-UTF-8 index path".to_string(),
        })?;
        self.inner
            .save(path_str)
            .map_err(|e| MemoryError::Index(format!("save: {}", e)))?;

        // Persist the key map alongside the index.
        let keys_path = format!("{}.keys.json", path_str);
        let json = self.key_map.to_json()?;
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
        let key_map = if std::path::Path::new(&keys_path).exists() {
            let json = std::fs::read_to_string(&keys_path)?;
            KeyMap::from_json(&json)?
        } else {
            KeyMap::new()
        };

        Ok(Self {
            inner: Arc::new(index),
            key_map,
        })
    }
}
