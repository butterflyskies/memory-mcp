# TODO

## Phase 1: Core + Semantic from the start

- [x] Scaffold Rust MCP server with streamable HTTP transport (PR #1)
- [x] Define memory file format (markdown + YAML frontmatter for tags, timestamps, source)
- [x] Implement memory repo management (init, open existing, commit) — git2
- [x] Choose local embedding model — fastembed (AllMiniLML6V2)
- [x] Build HNSW vector index alongside storage — usearch with cosine metric
- [x] Implement `remember` — embed, index (atomic via `add_with_next_key`), commit to git
- [x] Implement `recall` — semantic search with scope filtering, limit clamping, over-fetch
- [x] Implement `forget` — remove from index, delete file, commit
- [x] Implement `edit` — partial updates, skip re-embed when only tags change
- [x] Implement `list` — browse memories with optional scope filter
- [x] Implement `read` — read specific memory by name with full metadata
- [x] Implement `sync` — pull/push orchestration (stubs wired, auth flow pending)
- [x] Structured observability — tracing spans with timing on all handlers (ADR-0006)
- [x] Input validation — name validation, content size limits, nesting depth limits
- [x] Error mapping — `From<MemoryError> for ErrorData` with appropriate MCP error codes

## Phase 2: Sync + Auth + Migration

- [x] Implement real git push/pull with remote auth (PR #4)
  - [x] Lazy token resolution — local-only mode works without credentials
  - [x] Recency-based conflict resolution (ADR-0007)
  - [x] Configurable branch name (ADR-0009)
  - [x] Path-traversal and symlink protection in conflict resolution
  - [x] Integration tests (20 new, all offline with local bare remotes)
- [x] Incremental index rebuild on pull (PR #7)
  - [x] Diff old/new HEAD trees, re-embed only changed files
  - [x] VectorIndex::remove name_map corruption fix
  - [x] Refactor pull() into smaller named helpers
- [x] Keyring-based token storage (PR #9, ADR-0010)
  - [x] Resolution chain: env var → token file → system keyring
  - [x] Graceful degradation for headless/k8s (NoStorageAccess)
- [x] Auth subcommand with OAuth device flow (PR #10, ADR-0011, ADR-0012)
  - [x] CLI restructure: `serve` (default), `auth login`, `auth status`
  - [x] GitHub OAuth device flow with scoped token acquisition
  - [x] Token storage: keyring default, explicit `--store file|stdout` opt-in
  - [x] Security hardening: umask 0o077, atomic file writes, request/loop timeouts
- [ ] `--store k8s-secret` backend (cargo feature-gated)
- [ ] Extract OAuth client ID from hardcoded const into config module
- [ ] Integration tests for `auth login`, `auth status`, `MEMORY_MCP_BIND` env var
- [ ] Migration tool: import Serena global memories (preserve content + metadata) (ADR-0008)
- [ ] Migration tool: import Serena project-scoped memories
- [ ] Migration tool: import Claude Code auto-memories
- [ ] Container image (Dockerfile, publish to ghcr.io) — PR #13
- [ ] K8s deployment: Cilium Gateway API, StepClusterIssuer certs
- [ ] Container signing (cosign) — keyless vs long-lived key ADR, verify in deploy pipeline
- [ ] CVE scanning gate in CI — consume SBOM attestation with Grype/Trivy, block on critical
- [ ] Test cross-device sync workflow
- [ ] Configure as MCP server in `~/.claude.json`
- [ ] Update CLAUDE.md instructions to use memory-mcp instead of Serena memories

## Phase 3: Polish

- [ ] Deduplication / update detection on `remember` (semantic similarity threshold)
- [ ] Memory metadata enrichment (last-accessed, access count, confidence)
- [ ] Periodic background sync
- [ ] CLI for manual memory management outside of agent sessions
- [ ] Tag-based filtering in `recall` (currently semantic-only; tags are stored but not queried)
