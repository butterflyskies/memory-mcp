# ADR-0025: Two-Level Dispatch for Vector Index Abstraction

## Status
Accepted

## Context
Issue #94 introduces a `VectorStore` trait to decouple handlers from usearch, and a
private `RawIndex` trait inside the usearch implementation for failure injection testing.
Both are abstraction boundaries, but they serve different audiences and have different
performance profiles.

## Decision
- **`VectorStore` (semantic level):** dynamic dispatch via `Box<dyn VectorStore>` in
  `AppState`. Avoids generic parameter proliferation through the Axum handler stack.
  Matches the existing `Box<dyn EmbeddingBackend>` pattern.
- **`RawIndex` (implementation level):** static dispatch via generics with a default
  type parameter (`VectorIndex<R: RawIndex = UsearchRawIndex>`). Production code
  monomorphizes to zero-cost; test code substitutes `FailingRawIndex`.

## Consequences
- No runtime overhead for failure injection infrastructure in production builds.
- `VectorStore` pays one vtable indirection per call, which is negligible relative to
  the HNSW search or mutex acquisition it wraps.
- `RawIndex` is private — the generic parameter never leaks into public API.

## References
- #94
