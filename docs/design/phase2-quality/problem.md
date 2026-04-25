<!-- design-meta
status: draft
last-updated: 2026-04-24
phase: 1
-->

# Problem Space — Phase 2 Quality: Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

## What problem exists today?

memory-mcp has four interconnected quality and operational gaps remaining in
Phase 2:

### 1. No abstraction boundary for the vector index (#94)

`VectorIndex` and `ScopedIndex` call usearch's `Index` directly. This creates
two problems:

- **Untestable rollback path.** `ScopedIndex::add` performs a dual-index write
  (scope index, then all-index). If the all-index insert fails after the scope
  insert succeeds, it rolls back the scope insert. This rollback is the primary
  correctness guarantee for the dual-index write pattern, but usearch allocation
  failures are not injectable — so the path has zero test coverage.

- **No backend substitutability.** The rest of the system (server, MCP handlers,
  embedding pipeline) is coupled to usearch's API shape. Alternative backends
  (SQLite-backed, remote vector DB, in-memory for testing) would require
  rewriting every consumer.

The right fix is a **public trait at the semantic level** — what the system needs
from "vector storage" (add/remove/search by name and scope, persist, report
health) — with `VectorIndex`/`ScopedIndex` becoming implementation details of
a usearch-backed concrete type. A separate, private low-level trait inside the
usearch implementation enables failure injection for rollback testing.

### 2. Hardcoded OAuth client ID (#145)

`GITHUB_CLIENT_ID` is a `const` in `auth.rs`. This prevents:
- Testing the OAuth device flow against a mock server (different client ID)
- Using an alternative OAuth app without recompilation
- Integration tests that exercise the full auth path

### 3. No integration tests for auth CLI or bind address (#146)

The auth tests in `auth.rs` are unit-level: env var resolution, token file
storage. They don't cover:
- `auth login` end-to-end (device flow against a mock OAuth server)
- `auth status` output correctness
- `MEMORY_MCP_BIND` environment variable honored by the server

These paths are exercised only manually.

### 4. No readiness signal for orchestrators (#164)

`/healthz` returns 200 if the process is alive. It says nothing about whether
the server can serve requests. In the goddess cluster (or any deployment behind
a gateway), traffic can be routed to an instance where the repo is inaccessible,
the embedding model failed to load, or the vector index is corrupt.

A `/readyz` endpoint should check subsystem health and return structured results:
- Git repo: working directory accessible, HEAD resolvable
- Embedding model: loaded, able to produce vectors
- Vector index: loaded, non-zero dimensions

## Who experiences these problems?

| Actor | Impact |
|-------|--------|
| Developers | Cannot test rollback correctness, cannot integration-test auth, cannot confidently refactor the index layer |
| Operators | Cannot distinguish "process alive" from "service ready" in k8s/gateway deployments |
| CI pipeline | Has no way to catch auth regressions, bind-address issues, or index abstraction breaks |

## Why now?

Phase 2 is the quality phase. The tracing scaffold landed in v0.8.0, providing
structured observability across all subsystems. These four items complete the
"testability and operational readiness" story before Phase 3 (transport) and
Phase 4 (search improvements).

The index trait (#94) is also a prerequisite for Phase 4 work — ModernBERT
upgrade (#141) and memory chunking (#140) will be easier to develop and test
against a well-defined index abstraction.

## Inputs and outputs

| Item | Inputs | Outputs |
|------|--------|---------|
| #94 index trait | Embedding vectors, memory names, scopes | Search results, add/remove/save — same operations, behind a trait |
| #145 config | Config source (env var, CLI flag, compile-time default) | OAuth client ID available to auth module |
| #146 integration tests | Test harness with mock OAuth server | Pass/fail assertions on auth login, auth status, bind address |
| #164 /readyz | HTTP GET from orchestrator/gateway | 200 + JSON (ready) or 503 + JSON (which subsystem failed) |

## Boundaries

### In scope
- Public trait for vector storage at the semantic level (name + scope operations)
- Private trait inside usearch implementation for failure injection testing
- Usearch-backed implementation of the public trait (refactoring existing code)
- Config extraction for OAuth client ID
- Integration test harness for auth CLI subcommands and bind address
- `/readyz` endpoint with subsystem health checks
- Health-check capability on the vector storage trait (supports /readyz)

### Out of scope
- Alternative index backend implementations beyond test mocks
- Full config framework (TOML/YAML config files)
- `/metrics` endpoint (#165) — separate design surface
- Auth framework redesign (#79, Phase 6)
- W3C Trace Context (#162, Phase 5)

### Constraints
- The usearch index path must continue working — the trait is additive
- `/readyz` must not introduce new dependencies
- Integration tests must run in CI without real GitHub OAuth
- The public trait must not preclude fundamentally different backends (SQLite,
  remote vector DB) — design for substitutability, not just usearch's shape
- Both testing levels needed: private failure injection for usearch rollback
  mechanics, and public trait-level error behavior tests

## Success criteria

1. `ScopedIndex::add` rollback path has a direct test via injected failure
2. The rest of the codebase (server, MCP handlers) programs against the public
   vector storage trait, not usearch types
3. Auth integration tests cover device flow (mocked), status reporting, and
   bind-address override — all run in CI
4. `/readyz` returns structured health for git repo, embedding model, and vector
   index; the goddess deployment can adopt it as a readiness probe
5. All four items ship cohesively without requiring architectural rework in
   later phases
