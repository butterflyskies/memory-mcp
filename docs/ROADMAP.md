# memory-mcp Roadmap

> Canonical source of truth for the memory-mcp development plan.
> Updated with each phase completion. See the [pinned roadmap issue](../../issues) for discussion and progress notes.

## Design Principle

Earlier phases should be implemented with awareness of where the project is headed. Interfaces, error types, and module boundaries introduced in Phase 1-2 should accommodate the multi-user, multi-transport, observable future without over-engineering for it now.

---

## Phase 1: Stabilize & Quick Wins

**Goal:** Fix known bugs, improve agent UX, and clear low-hanging fruit.

| Issue | Title | Effort |
|-------|-------|--------|
| #88 | Flaky test: FastForward vs Merged | Small |
| #81 | git push silently succeeds on reject | Small |
| #69 | Atomic file writes + cleanup | Medium |
| #106 | Recall truncation guidance for agents | Small |
| #108 | Secret-avoidance in tool instructions | Small |
| #105 | Document `docker run` pattern in README | Small |

**Forward-looking notes:**
- #69 (atomic writes): Design as a reusable utility for index persistence, config files, and multi-user writes.
- #106 (recall results): Structure the response type to be extensible for future audit logging and access identity fields.

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

**Goal:** Richer retrieval capabilities and memory lifecycle management.

| Issue | Title | Effort | Depends on |
|-------|-------|--------|------------|
| #55 | BM25 keyword search via Tantivy | Large | -- |
| #107 | Memory expiry: TTL + completion-triggered deletion | Medium | -- |

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

## Priority Order

```
Phase 1 (Stabilize)           ███░░░░░░░░░░░░░░░░░  Now
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
