# TODO

## Phase 1: Core + Semantic from the start

- [x] Scaffold Rust MCP server with streamable HTTP transport (PR #1)
- [x] Define memory file format (markdown + YAML frontmatter for tags, timestamps, source)
- [x] Implement memory repo management (init, open existing, commit) — git2
- [x] Choose local embedding model — ~~fastembed~~ candle direct (BGE-small-en-v1.5, ADR-0016)
- [x] Build HNSW vector index alongside storage — usearch with cosine metric
- [x] Implement `remember` — embed, index (atomic via `add_with_next_key`), commit to git
- [x] Implement `recall` — semantic search with scope filtering, limit clamping, over-fetch
- [x] Implement `forget` — remove from index, delete file, commit
- [x] Implement `edit` — partial updates, skip re-embed when only tags change
- [x] Implement `list` — browse memories with optional scope filter
- [x] Implement `read` — read specific memory by name with full metadata
- [x] Implement `sync` — pull/push orchestration with remote auth
- [x] Structured observability — tracing spans with timing on all handlers (ADR-0006)
- [x] Input validation — name validation, content size limits, nesting depth limits
- [x] Error mapping — `From<MemoryError> for ErrorData` with appropriate MCP error codes

## Phase 2: Sync + Auth + Deployment

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
- [x] `--store k8s-secret` backend (cargo feature-gated, `--features k8s`)
- [x] Container image (Dockerfile, publish to ghcr.io) — PR #13
- [x] K8s deployment manifests (`deploy/k8s/`)
- [ ] Extract OAuth client ID from hardcoded const into config module
- [ ] Integration tests for `auth login`, `auth status`, `MEMORY_MCP_BIND` env var

## Phase 2.5: Trust signals + crates.io

- [x] Cargo.toml metadata (description, repository, keywords, categories)
- [x] cargo-deny replacing cargo-audit in CI
- [x] `#![warn(missing_docs)]` on lib crate
- [x] MSRV 1.88 declared + CI enforced (ADR-0017)
- [x] Dedicated GitHub App for release-please (ADR-0018)
- [x] Fat lib / thin binary refactor — public API surface (PR #64, closes #61)
- [x] Replace `Secret<T>` with `secrecy` crate — zeroize-on-drop (#66)
- [x] Semver hardening — `#[non_exhaustive]`, constructors, serde strategy (ADR-0019)
- [x] Proper path resolution with `homedir` + `shellexpand` (#68)
- [x] cargo-semver-checks in CI (PR #73)
- [x] Published to crates.io (v0.3.0)
- [ ] Trusted Publishing (OIDC from GitHub Actions) (#62)
- [ ] Atomic file writes and symlink safety in token I/O (#69)
- [ ] cargo-auditable — embed dep tree in release binaries (#60, blocked on upstream)

## Phase 3: Retrieval + Polish

- [ ] BM25 keyword search via Tantivy (#55)
- [ ] Cross-platform vector index — brute-force fallback for Windows (#56)
- [ ] Deduplication / update detection on `remember` (semantic similarity threshold)
- [ ] Tag-based filtering in `recall` (tags are stored but not queried)
- [ ] Memory metadata enrichment (last-accessed, access count, confidence)
- [ ] Periodic background sync
- [ ] Comprehensive tracing and observability pass (#52)
- [ ] recall over-fetch multiplier — handle truncated results (#71)

## Future

- [ ] Migration tools: import from Serena memories, Claude Code auto-memories (ADR-0008)
- [ ] Container signing (cosign)
- [ ] CVE scanning gate in CI (Grype/Trivy on SBOM attestation)
- [ ] CLI for manual memory management outside of agent sessions
- [ ] Configure as default MCP server in `~/.claude.json` / Cursor / Windsurf
