# ADR-0022: Scope-Partitioned Vector Indexes

## Status
Accepted

## Context
The single shared vector index forces post-search scope filtering in `recall`, causing silent result truncation when the over-fetch multiplier (3x) is insufficient (#71). With scope affinity (ADR-0021) making scope filtering the default path, every `recall` call now hits this limitation. Reads far dominate writes, so the index architecture should optimize for read performance.

## Decision
Partition the vector index by scope: one `VectorIndex` per scope (global, each project) plus a combined "all" index. Each memory is indexed in both its scope-specific index and the "all" index. `recall` routes queries to the relevant indexes based on `ScopeFilter`, eliminating post-search filtering entirely. Index freshness is tracked via git commit SHA stored in each index's metadata.

## Consequences
- Reads are exact: no over-fetch multiplier, no silent result truncation, no wasted candidates
- Write cost doubles (two index inserts per memory) — acceptable given read-dominant workload
- Disk usage increases proportionally (per-scope files + combined)
- Startup performs eager scope scan with SHA-based incremental rebuild
- Old single-index format is deleted and rebuilt on first startup after upgrade
