# memory-mcp Roadmap

> Canonical source of truth for the memory-mcp development plan.
> Updated with each phase completion. See the [pinned roadmap issue](https://github.com/butterflyskies/memory-mcp/issues/132) for discussion and progress notes.

## Design Principle

Earlier phases should be implemented with awareness of where the project is headed. Interfaces, error types, and module boundaries introduced in Phase 1-2 should accommodate the multi-user, multi-transport, observable future without over-engineering for it now.

---

## Completed Work

### Phase 1: Stabilize & Quick Wins

**Completed 2026-04-12.** All 6 issues closed. Released in v0.6.0 and v0.6.1.

| Issue | Title | Status |
|-------|-------|--------|
| #88 | Flaky test: FastForward vs Merged | v0.5.1 |
| #81 | git push silently succeeds on reject | v0.6.0 |
| #69 | Atomic file writes + cleanup | v0.6.1 |
| #106 | Recall truncation guidance for agents | v0.6.0 |
| #108 | Secret-avoidance in tool instructions | v0.6.0 |
| #105 | Document `docker run` pattern in README | v0.6.0 |

**Design decisions carried forward:**
- `MemoryError` is `#[non_exhaustive]` — future variants are patch-compatible.
- `fs_util::atomic_write` is `pub(crate)` — reusable for index persistence and config writes in later phases.
- Recall response includes `truncated` flag and `content_length` — extensible for audit logging fields.

### Earlier Phases (Pre-Roadmap)

Core features delivered in v0.1.0–v0.5.0:
- MCP server with streamable HTTP transport
- Memory file format (markdown + YAML frontmatter)
- All core tools: `remember`, `recall`, `forget`, `edit`, `list`, `read`, `sync`
- Local embedding (candle, BGE-small-en-v1.5) + HNSW vector index (usearch)
- Git push/pull with remote auth, incremental index rebuild
- Keyring-based token storage, OAuth device flow
- K8s deployment manifests, container image
- Fat lib / thin binary refactor, semver hardening
- Published to crates.io (v0.3.0+)

Closed issues from original TODO: #60 (cargo-auditable), #62 (Trusted Publishing), #69 (atomic writes), #71 (recall over-fetch).

---

## Phase 2: Tracing Scaffold + Core Quality

**Goal:** Lay the observability foundation early so all subsequent work is automatically instrumented. Improve dev workflow and testing infrastructure.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #52 (partial) | Tracing scaffold | Medium | -- |
| #114 | Surface keep-alive timeout to clients | Small | -- |
| #94 | Vector index trait abstraction | Medium | -- |
| #109 | Content-level secret scanning | Medium | #108 |
| #98 | Automate CHANGELOG without bypassing branch protection | Medium | -- |
| #78 | `validate` subcommand + Docker build speedup | Large | -- |
| #145 | Extract OAuth client ID from hardcoded const into config module | Small | -- |
| #146 | Integration tests for auth login, auth status, MEMORY_MCP_BIND | Medium | -- |

**Key insight:** #52 (comprehensive tracing) is cross-cutting. The scaffold goes in Phase 2 so every new feature gets spans for free. Structured fields: operation, scope, memory_name, duration. OTLP export behind feature flag.

**Design the vector index trait (#94) with future backends in mind:** brute-force fallback (#56), Tantivy BM25 (#55).

---

## Phase 3: Transport & Stdio

**Goal:** Add stdio as a secondary transport for local single-user deployments.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #104 | Stdio transport | Medium | -- |
| #103 | Non-GitHub git remotes | Large | -- |

**ADR required:** New ADR superseding ADR-0001. Stdio for local single-user (Claude Code manages process). HTTP for deployed/multi-user. Mutual exclusion: when serving HTTP(S), stdio/stdout is disabled.

---

## Phase 4: Search & Memory Lifecycle

**Goal:** Richer retrieval capabilities, better embedding quality, and memory lifecycle management.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #141 | Upgrade to ModernBERT Embed (8192 token context) | Medium | -- |
| #140 | Chunk long memories for embedding | Medium | #141 |
| #55 | BM25 keyword search via Tantivy | Large | -- |
| #107 | Memory expiry: TTL + completion-triggered deletion | Medium | -- |
| #147 | Deduplication / update detection on `remember` | Medium | -- |
| #148 | Tag-based filtering in `recall` | Small | -- |
| #149 | Memory metadata enrichment (last-accessed, access count, confidence) | Medium | -- |
| #150 | Periodic background sync | Medium | -- |

**Key insight:** BGE-small-en-v1.5 silently truncates memories beyond 512 tokens (~400 words). ModernBERT Embed (`nomic-ai/modernbert-embed-base`) provides a 16x context window (8192 tokens), alternating attention for CPU efficiency, and Matryoshka dimensions. Already implemented in candle-transformers. Chunking (#140) handles memories that still exceed the new window. BM25 (#55) provides a complementary retrieval path with no token limit.

---

## Phase 5: Audit Logging (completing #52)

**Goal:** Build on the tracing scaffold from Phase 2 to add compliance-ready audit fields.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #52 (remainder) | Complete tracing pass | Medium | Phase 2 scaffold |
| #110 | Authenticated user identity in log events | Small | Phase 2 scaffold |
| #111 | Log returned memory names on recall | Small | Phase 2 scaffold |
| #112 | Log auth events | Small | Phase 2 scaffold |
| #117 | Git commit SHA in write operation logs | Small | Phase 2 scaffold |

---

## Phase 6: Enterprise & Multi-User

**Goal:** Transform from single-user tool into a multi-user platform.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #115 | Authentication & authorization framework | Large | #52 |
| #116 | Memory scope isolation | Large | #115 |
| #113 | Log access denied events | Small | #115 |
| #119 | Enterprise scope hierarchy (org/team/project/user) | Large | #116 |
| #118 | Production deployment checklist | Medium | #115, #116 |

---

## Phase 7: Auth & Platform Polish

**Goal:** Reduce operational burden, expand platform reach. #79 can interleave with any earlier phase.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #79 | GitHub App installation token auth | Medium | -- |
| #42 | Windows: usearch MAP_FAILED | Medium | -- |
| #56 | Cross-platform vector index (brute-force fallback) | Medium | #42, #94 |

Windows support (#42, #56) is deferred until demand materializes.

---

## Future

Items without a phase assignment yet. These will be scheduled as priorities become clear.

| Issue | Title | Category |
|-------|-------|----------|
| #129 | Memory consolidation, write discipline, and quality metrics | Quality |
| #130 | Post-compaction context re-priming hook | Agent UX |
| #144 | Migrate optional ureq dep from v2 to v3 | Dependencies |
| #151 | Migration tools for Serena memories and Claude Code auto-memories | Migration |
| #152 | Container signing with cosign | Security |
| #153 | CVE scanning gate in CI (Grype/Trivy) | Security |
| #154 | CLI for manual memory management outside agent sessions | UX |
| #161 | Revisit candle fork patch when upstream merges | Dependencies |


---

## Priority Order

```
Phase 1 (Stabilize)           ████████████████████  Done (v0.6.0/v0.6.1)
Phase 2 (Tracing + Quality)   ░░░████░░░░░░░░░░░░░  Next
  #79 (GitHub App Auth)       ░░░░░██░░░░░░░░░░░░░  Interleave anytime
Phase 3 (Transport/Stdio)     ░░░░░░░███░░░░░░░░░░  After core quality
Phase 4 (Search/Lifecycle)    ░░░░░░░░░░███░░░░░░░  After transport
Phase 5 (Audit Logging)       ░░░░░░░░░░░░░██░░░░░  Completing tracing
Phase 6 (Enterprise)          ░░░░░░░░░░░░░░░████░  Last (largest, most deps)
Phase 7 (Windows/polish)      ░░░░░░░░░░░░░░░░░░░░  On demand
```

## /design Workflow

- **Phase 1:** No /design needed -- straightforward bug fixes.
- **Phase 2+:** Run /design with the phase scope 1-2 sessions before starting. Feed it the phase table plus awareness of downstream phases.
- After each phase lands, update this document with what changed and design decisions that affect later phases.
