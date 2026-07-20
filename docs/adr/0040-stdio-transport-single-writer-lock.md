# ADR-0040: stdio Transport for Single-User Local Deployments

## Status
Accepted. Supersedes ADR-0001 in part: Streamable HTTP is no longer the sole
transport, but remains the default and the only supported transport for
networked/multi-client deployments.

## Context
ADR-0001 chose Streamable HTTP as the only transport, sized for k8s and
multi-machine access. The first external adopters run the server on the same
machine as their MCP client, where HTTP means managing a background daemon,
a bind address, and a Host-header allowlist — all friction with no benefit
for a single local user. MCP clients (including Claude Code) manage process
lifecycle natively when a server speaks stdio (#104).

stdio changes the concurrency model: one process per client, but all
processes share one repo directory. The git repo, the usearch index files,
and the recall log are owned exclusively by a single process today — sqlite
has a busy timeout, but git commits and the shutdown index save do not
tolerate a second writer.

## Decision
- Add `serve --transport {http|stdio}` (env `MEMORY_MCP_TRANSPORT`), default
  `http`. stdio serves one client over stdin/stdout via rmcp's stdio
  transport; stdout carries only JSON-RPC framing (tracing is stderr-only).
- Enforce a single writer per repository with an OS advisory lock
  (`flock` semantics) on `<repo>/.memory-mcp-index/.lock`, acquired before
  any subsystem opens. A second server process — stdio or HTTP — fails fast
  with an error naming the holder's pid. No waiting, no retries.

## Rejected alternatives
- **Multi-writer coordination** (shared locks around git/index/recall-log
  operations): large surface, and cross-process cache coherence for the
  in-RAM vector and lexical indexes would still be unsolved. A second local
  process is a deployment error, not a use case.
- **Lease/TTL lockfiles**: stale-lock heuristics and clock dependence for a
  problem the kernel already solves — an advisory lock is released on process
  exit, including crashes, and can never go stale.
- **Documenting a single-client constraint** without enforcement: silent
  index or repo corruption when the constraint is violated accidentally
  (e.g. a stdio instance started while the HTTP daemon runs).

## Consequences
- Local single-user setups need no daemon, no port, no Host allowlist; the
  client owns the process lifecycle and stdin EOF is a clean shutdown (the
  vector index is persisted on exit, as with HTTP).
- One process per client means one embedding model load per client; anyone
  needing concurrent clients or networked access should run the HTTP server.
- The lock applies to HTTP too: two HTTP daemons on one repo now fail fast
  instead of corrupting state — a latent foot-gun closed.
- Session-scoped observability degrades gracefully under stdio: there is no
  `Mcp-Session-Id` header, so spans carry the literal session id `stdio`.
