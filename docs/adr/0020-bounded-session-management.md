# ADR-0020: Bounded session management via mcp-session

## Status
Accepted

## Context
MCP sessions are created per client `initialize` request and stored in-memory by rmcp's
`LocalSessionManager`. Without bounds, sessions accumulate indefinitely — a resource
exhaustion vector. The MCP spec recommends session expiry; industry practice requires
both idle timeouts and max session counts for any stateful server.

Options considered:
1. **Configure timeout only** — `keep_alive` on `LocalSessionManager`. Simple but doesn't
   bound burst/attack scenarios (thousands of sessions in the timeout window).
2. **Use a bounded session manager** — Wrap `LocalSessionManager` with max session count,
   FIFO eviction, and rate limiting. Extract as a reusable crate.
3. **Custom `SessionManager` from scratch** — Full reimplementation. Unnecessary complexity
   when `LocalSessionManager` already handles session lifecycle correctly.
4. **Reject new sessions at capacity** — Return an error instead of evicting. Simpler but
   risks legitimate clients being locked out by stale sessions that haven't timed out yet.

## Decision
Option 2: use the [`mcp-session`](https://crates.io/crates/mcp-session) crate, which
provides `BoundedSessionManager` — a `SessionManager` wrapper that enforces session
timeout (via `SessionConfig::keep_alive`), max session count (via FIFO eviction), and
rate limiting on session creation. FIFO eviction chosen over LRU because session activity
tracking would require instrumenting every method call, and oldest-first is a reasonable
proxy — genuinely active sessions will have been created more recently or will reconnect
after eviction.

The session management layer was extracted to a standalone crate
([butterflyskies/mcp-session](https://github.com/butterflyskies/mcp-session)) so other
MCP server implementations can reuse it.

## Consequences
- Both steady-state (idle cleanup) and adversarial (burst creation) scenarios are bounded
- Legitimate clients evicted under load can reconnect and get a new session
- Max sessions configurable via `--max-sessions` (default 100)
- Rate limiting configurable via `--session-rate-limit` / `--session-rate-window-secs`
- Split critical sections — no locks held across async operations
- Session timeout set to 4 hours (rmcp's `keep_alive` is absolute, not idle-based)
