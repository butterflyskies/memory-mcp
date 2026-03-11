# ADR-0003: usearch for Vector Indexing

## Status
Accepted

## Context
Semantic recall requires a vector index for nearest-neighbor search over memory embeddings.
Options considered: hand-rolled brute-force cosine similarity, `usearch` (C++ HNSW with Rust
bindings), `hnsw_rs` (pure Rust), `instant-distance` (unmaintained), `hora` (stalled).

## Decision
Use `usearch` as the primary vector index. Fall back to `hnsw_rs` if usearch's Rust bindings
have build or API issues.

## Consequences
- Established library with company backing (Unum), broad language support, active maintenance
- HNSW algorithm provides sub-linear search time — scales beyond brute-force for large memory sets
- Save/load to disk supported — index persists across restarts (gitignored, rebuilt if missing)
- C++ dependency requires CMake at build time
- Integration is lower-risk than rolling our own similarity search
