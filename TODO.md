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

## Phase 2: Sync + Migration

- [x] Implement real git push/pull with remote auth (PR #4)
  - [x] Lazy token resolution — local-only mode works without credentials
  - [x] Recency-based conflict resolution (ADR-0007)
  - [x] Configurable branch name (ADR-0009)
  - [x] Path-traversal and symlink protection in conflict resolution
  - [x] Integration tests (20 new, all offline with local bare remotes)
- [ ] Keyring-based token storage via `keyring` crate (sync-secret-service for KWallet/GNOME Keyring)
- [ ] Index rebuild on pull (incremental if possible)
- [ ] Migration tool: import Serena global memories (preserve content + metadata) (ADR-0008)
- [ ] Migration tool: import Serena project-scoped memories
- [ ] Migration tool: import Claude Code auto-memories
- [ ] Configure as MCP server in `~/.claude.json`
- [ ] Update CLAUDE.md instructions to use memory-mcp instead of Serena memories
- [ ] Test cross-device sync workflow

## Phase 3: Polish

- [ ] Deduplication / update detection on `remember` (semantic similarity threshold)
- [ ] Memory metadata enrichment (last-accessed, access count, confidence)
- [ ] Periodic background sync
- [ ] CLI for manual memory management outside of agent sessions
- [ ] Tag-based filtering in `recall` (currently semantic-only; tags are stored but not queried)
