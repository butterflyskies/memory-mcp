<!-- design-meta
status: approved
last-updated: 2026-05-14
phase: 1
-->

# Problem Space: Session Lifecycle Observability (#114)

## What problem exists today?

When an MCP session expires due to keep-alive timeout, the server logs:

```
worker quit with fatal: keep alive timeout after 14400000ms
```

The client receives no signal. The SSE stream silently closes. The client
discovers the session is dead only when its next request returns HTTP 404 with
a plain-text body that carries no structured information or reconnection
guidance.

This hits AI agents hardest: they believe the connection is live, attempt a
tool call, get a raw 404, and enter unpredictable error-recovery behavior.

## Why now?

memory-mcp is deployed in long-running production environments serving AI
agents that hold sessions for hours. The 4-hour keep-alive is a reasonable
safety net, but the silent death when it fires creates operational confusion.
Additionally, mcp-session has consumers beyond memory-mcp (~420 non-memory-mcp
downloads) who face the same problem with no tooling to address it.

## Architectural constraint

The keep-alive timeout mechanism lives inside rmcp's `LocalSessionWorker::run()`
loop. The timer is created fresh at the top of each loop iteration — it is
actually an **idle timer** that resets on every message, not an absolute
lifetime. When no events arrive for the configured duration, the worker returns
`WorkerQuitReason::Fatal(KeepAliveTimeout(...))` — no callback, no event, the
session dies. mcp-session wraps session counting and rate limiting but has no
visibility into or control over session lifecycle events.

rmcp's idle timer is already correct and elegant — it creates a fresh
`tokio::time::sleep()` at the top of each `select!` loop iteration, so any
event naturally resets it with zero overhead. Reimplementing this outside the
event loop would be strictly worse.

The approach is to **lean on rmcp for idle timeout** (passing the configured
value through `SessionConfig::keep_alive` via mcp-session's builder) and add
**max lifetime as a genuinely new capability** via a one-shot per-session
tokio task. mcp-session's builder absorbs the `keep_alive` field from
`SessionConfig` to prevent the two-knob problem, but passes the value through
rather than replacing the timer.

## Inputs and outputs

**Inputs:**
- Idle timeout duration (currently hardcoded in each consumer, delegated to rmcp)
- Max session lifetime (new — absolute cap regardless of activity)
- Session creation/destruction events (currently opaque inside rmcp)

**Outputs:**
- Structured session lifecycle logs (created, closed with reason and duration)
- Configurable idle timeout and max lifetime via CLI flags / environment variables
- Clean session shutdown with close reason tracking

## Scope

### In scope

- **mcp-session API changes**: builder pattern, idle timeout and max
  lifetime configuration, session lifecycle tracing with close reasons
- **Idle timeout**: passed through to rmcp's `SessionConfig::keep_alive`
  (rmcp's event-loop timer handles the actual idle detection)
- **Max lifetime**: new per-session one-shot tokio task, cancelled on
  early close via `AbortHandle`
- **SessionConfig absorption**: mcp-session's builder owns `keep_alive`;
  other `SessionConfig` fields pass through. Conflicting `keep_alive` on
  a passthrough `SessionConfig` is overridden with a logged warning.
- **memory-mcp CLI**: `--idle-timeout-secs` and `--max-session-lifetime-secs`
  flags with corresponding env vars
- **Backward compatibility**: consumers who upgrade mcp-session without
  changing code see no behavior change. The builder is opt-in. If
  `.idle_timeout()` is not called, the `SessionConfig` value flows
  through to rmcp unmodified.

### Out of scope

- Changes to rmcp itself
- Client-side reconnection logic (we provide the signal; the client decides
  what to do with it)
- Session persistence or resumption across timeout (separate feature)
- Specific deployment environment details

## Adjacent systems

- **rmcp** (v1.7.0): provides `LocalSessionManager`, `SessionConfig`,
  `StreamableHttpService`, the worker loop with keep-alive timer
- **mcp-session** (v0.2.0): wraps `LocalSessionManager` with bounded
  sessions, rate limiting, FIFO eviction, lifecycle tracking. Re-exports
  `SessionConfig`.
- **memory-mcp**: the primary consumer; constructs `BoundedSessionManager`
  and `StreamableHttpService`, defines CLI flags

## Success criteria

1. Session lifecycle events (created, closed) are logged with `session_id`,
   duration, and close reason via structured tracing.
2. Idle timeout and max session lifetime are independently configurable via
   CLI flags and environment variables.
3. The mcp-session API change is additive — existing consumers who don't
   opt into managed timeouts see no behavior change.
4. No timer conflict: idle timeout flows through a single configuration
   path (builder → SessionConfig → rmcp).

## Failure modes

- **Breaking existing consumers**: if the API change requires code changes
  from consumers who don't want the new feature. Prevented by keeping the
  existing constructor and making the builder opt-in.
- **Orphaned max-lifetime tasks**: if a session is closed (eviction, client
  DELETE, server shutdown) but the one-shot max-lifetime task is not
  cancelled, it fires on a dead session. Prevented by cancelling via
  `AbortHandle` in `close_session`.
- **Two-knob conflict**: if a consumer sets `keep_alive` on both the
  builder and a passthrough `SessionConfig`. Prevented by the builder
  overriding the passthrough value with a logged warning.
