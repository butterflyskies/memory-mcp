# ADR-0020: Bounded session management with FIFO eviction

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
2. **Wrap with bounded manager + timeout** — Delegate to `LocalSessionManager`, add a max
   session count with FIFO eviction of the oldest session when the cap is reached.
3. **Custom `SessionManager` from scratch** — Full reimplementation. Unnecessary complexity
   when `LocalSessionManager` already handles session lifecycle correctly.
4. **Reject new sessions at capacity** — Return an error instead of evicting. Simpler but
   risks legitimate clients being locked out by stale sessions that haven't timed out yet.

## Decision
Option 2: wrap `LocalSessionManager` in a `BoundedSessionManager` that enforces both idle
timeout (via `SessionConfig::keep_alive`) and max session count (via FIFO eviction). FIFO
chosen over LRU because session activity tracking would require instrumenting every method
call, and oldest-first is a reasonable proxy — genuinely active sessions will have been
created more recently or will reconnect after eviction.

## Consequences
- Both steady-state (idle cleanup) and adversarial (burst creation) scenarios are bounded
- Legitimate clients evicted under load can reconnect and get a new session
- Max sessions is configurable via `--max-sessions` CLI arg (default 100)
- Creation-order tracking adds a `Mutex<VecDeque<SessionId>>` — serializes session creation,
  acceptable for a single-instance server
