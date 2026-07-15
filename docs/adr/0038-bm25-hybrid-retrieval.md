# ADR-0038: Hybrid retrieval — BM25 lexical search fused with semantic recall

## Status
Accepted

## Context
Whole-memory embeddings dilute short exact phrases inside long
multi-topic memories: a one-line fact in a ~3 KB memory contributes
almost nothing to the document vector, so literal-phrase queries rank
it below short memories that are topically adjacent but wrong. A
concrete field failure is documented on issue #55: the query "happy
birthday" failed to surface the memory containing that verbatim
phrase, while every top-10 result was noise.

Alternatives considered:
- **Substring grep fallback** when semantic top-k distances are poor —
  cheap, but no ranking, no tokenisation, and a threshold to tune.
- **Chunk-level embeddings** (#140) — helps long memories generally
  but does not guarantee literal-phrase retrieval; complementary, not
  a substitute.
- **BM25 lexical search fused with semantic ranking** — standard
  hybrid retrieval; the two strategies fail independently.

## Decision
Add a BM25 keyword index (Tantivy, pure Rust — Lucene-inspired, no FFI)
alongside the vector index, and merge the two ranked lists with
reciprocal rank fusion (RRF, k = 60).

Key choices:
- **In-RAM index, rebuilt on startup.** Indexing text is cheap (unlike
  embedding it), so the Tantivy index is never persisted. This removes
  the entire index-versioning/migration surface; startup cost is one
  `list_memories` pass.
- **Same write path.** The handlers that update the vector index
  (remember, edit, forget, move, incremental reindex) mirror the write
  into the lexical index. Lexical writes are best-effort: a failure
  logs a warning and degrades that memory to semantic-only until the
  next startup rebuild. Mutations are batched (`LexicalOp` lists): each
  logical operation performs exactly one Tantivy commit and one reader
  reload — incremental reindex applies the whole changed set in one
  batch — and the commit runs on the Tokio blocking pool
  (`apply_async`), never on an async worker thread.
- **Exact-phrase precedence.** BM25 length normalisation lets a short
  document containing one query term outrank a long document containing
  the exact phrase — the very failure hybrid retrieval exists to fix.
  Lexical search therefore runs two passes: exact-phrase matches rank
  strictly above term-only matches. Fusion consumes ranks, not scores.
- **RRF over score interpolation.** Cosine distances and BM25 scores
  live on incomparable scales; rank fusion needs no normalisation, no
  tuned weights, and is fully deterministic (score ties break
  lexical-first — a hit with a lexical contribution outranks a tied hit
  without one — then on qualified name).
- **Error asymmetry.** A semantic failure is fatal (preserves recall's
  pre-hybrid error surface); a lexical failure degrades to
  semantic-only with a warning. `LexicalIndex::new()` is infallible —
  an initialisation failure yields a disabled instance whose operations
  all error, which callers already treat as degradation. This keeps
  `AppState::new` and startup unchanged.
- **Scope filtering** reuses `ScopeFilter::matches` on scopes parsed
  from the canonical index key, post-search, bounded at 10 000
  candidates — identical semantics to recall's existing filtering.

## Consequences
- Literal phrases buried in long memories are retrievable: the top
  lexical hit outranks every semantic-only candidate in the fused list
  — 1/(k+1) beats 1/(k+r) for r > 1, and the remaining rank-0 tie
  (lexical rank 0 vs semantic rank 0, both 1/(k+1)) resolves in the
  lexical hit's favour via the deterministic lexical-first tie-break.
  Only candidates found by *both* strategies can rank above it.
- Recall results gain a `match_type` field (`semantic` / `lexical` /
  `both`). `distance` stays a required numeric field for wire
  compatibility with pre-hybrid clients: lexical-only hits, which have
  no embedding distance, carry the `-1.0` sentinel (impossible as a
  real cosine distance). Recall-log entries use the same sentinel,
  which distance-bucketed stats already exclude (`distance >= 0.0`).
- Startup reads all memories once even when the vector index is fresh.
- The binary grows by the Tantivy dependency tree (pure Rust, no new
  C/C++ FFI beyond the existing zstd already vendored elsewhere).
- Memory cost: one in-RAM copy of all memory text plus posting lists —
  negligible next to the embedding model.
