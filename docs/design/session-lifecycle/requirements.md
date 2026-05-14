<!-- design-meta
status: approved
last-updated: 2026-05-14
phase: 2
-->

# Requirements: Session Lifecycle Observability (#114)

## Use Cases

| ID | Actor | Use Case | Type | Priority |
|----|-------|----------|------|----------|
| UC-01 | MCP Client | Hold a long-running session for tool calls | Normal | Must |
| UC-02 | Operator | Configure idle timeout and max lifetime for deployment | Normal | Must |
| UC-03 | Operator | Monitor session lifecycle events in logs | Normal | Must |
| UC-04 | Operator | Diagnose why a session ended (timeout vs eviction vs client close) | Normal | Should |
| AC-01 | Attacker | Hold sessions indefinitely to exhaust server resources | Abuse | Must-mitigate |
| SC-01 | System | Enforce session lifetime regardless of client behavior | Security | Must |
| SC-02 | System | Log session lifecycle with audit-quality detail | Security | Should |

### Notes on abuse cases

- **AC-01**: Mitigated by three independent mechanisms: `BoundedSessionManager`
  (max sessions + FIFO eviction), idle timeout (rmcp reaps inactive sessions),
  and max lifetime (absolute cap for chatty sessions). All three are independent.

## Key discovery: rmcp's keep_alive is already an idle timer

rmcp's `SessionConfig::keep_alive` creates a `tokio::time::sleep()` at the top
of each `select!` loop iteration in `LocalSessionWorker::run()`. Any event
resets it naturally. The timeout only fires after the full duration with zero
activity.

mcp-session's builder absorbs this field to prevent the two-knob problem, but
passes the value through to rmcp rather than reimplementing the timer. The
genuinely new capability is **max lifetime** — an absolute session cap
regardless of activity, implemented as a one-shot per-session tokio task.

## Requirements

### mcp-session (crate changes)

| Req ID | Requirement | Source UC | ASVS | Priority |
|--------|-------------|-----------|------|----------|
| R-01 | mcp-session SHALL provide a builder API for `BoundedSessionManager` that accepts idle timeout, max lifetime, and max sessions as parameters | UC-02 | — | Must |
| R-02 | The builder's `.idle_timeout(duration)` SHALL set `SessionConfig::keep_alive` to the given value on the inner `LocalSessionManager` | UC-01, SC-01 | V3.3 | Must |
| R-03 | If a passthrough `SessionConfig` has `keep_alive` set AND the builder's `.idle_timeout()` was called, mcp-session SHALL override the `SessionConfig` value and log a warning | UC-02 | — | Must |
| R-04 | If `.idle_timeout()` is not called on the builder, the passthrough `SessionConfig::keep_alive` SHALL flow through to rmcp unmodified | UC-01 | — | Must |
| R-05 | When max lifetime is configured, mcp-session SHALL spawn a one-shot tokio task per session that calls `close_session()` after the configured duration | SC-01, AC-01 | V3.3 | Must |
| R-06 | Max lifetime tasks SHALL be cancelled via `AbortHandle` when the session is closed for any other reason (eviction, client DELETE, idle timeout, server shutdown) | SC-01 | V3.3 | Must |
| R-07 | mcp-session SHALL emit a structured tracing event when a session is created, including `session_id` | UC-03, SC-02 | V7.1 | Must |
| R-08 | mcp-session SHALL emit a structured tracing event when a session is closed, including `session_id` and session duration (elapsed since creation) | UC-03, SC-02 | V7.1 | Must |
| R-09 | Session close tracing events SHALL include a close reason where determinable: `Evicted`, `MaxLifetime`, or `Closed` (catch-all for idle timeout, client DELETE, and other external closes) | UC-04 | V7.1 | Should |
| R-10 | Consumers who construct `BoundedSessionManager` via the existing `new()` + `with_rate_limit()` API SHALL see no behavior change | UC-01 | — | Must |
| R-11 | The builder SHALL accept optional `SessionConfig` for passthrough fields (`channel_capacity`, `sse_retry`, `completed_cache_ttl`, `init_timeout`); `keep_alive` is subject to R-03 and R-04 | UC-02 | — | Must |
| R-12 | The builder SHALL re-expose rate limiting configuration so all session management is configurable through a single builder chain | UC-02 | — | Should |

### memory-mcp (consumer changes)

| Req ID | Requirement | Source UC | ASVS | Priority |
|--------|-------------|-----------|------|----------|
| R-13 | memory-mcp SHALL expose `--idle-timeout-secs` as a CLI flag and `MEMORY_MCP_IDLE_TIMEOUT_SECS` env var, defaulting to 14400 (4 hours, matching current behavior) | UC-02 | — | Must |
| R-14 | memory-mcp SHALL expose `--max-session-lifetime-secs` as a CLI flag and `MEMORY_MCP_MAX_SESSION_LIFETIME_SECS` env var, defaulting to 0 (disabled) | UC-02 | — | Should |
| R-15 | memory-mcp SHALL construct `BoundedSessionManager` via the new builder API, passing the CLI-configured values | UC-02 | — | Must |

## ASVS Categories Reviewed

| Category | Applicable? | Rationale |
|----------|-------------|-----------|
| V3 Session Management | **Yes** | Core — session lifetime, idle timeout, max lifetime |
| V7 Error Handling, Logging | **Yes** | Lifecycle logging with structured tracing |
| V1 Architecture | Reviewed | Timer delegation to rmcp covered in problem.md |
| V2 Authentication | No | Session lifecycle is orthogonal to auth |
| V4 Access Control | No | No authorization changes |
| V5 Validation | No | No new input parsing beyond CLI flags (handled by clap) |
| V11 Business Logic | No | Session timeout is infrastructure |
| V13 API | No | No new client-facing API surface |

## Security Requirements Traceability Matrix

| Req ID | Requirement | Source UC | ASVS | Test Case |
|--------|-------------|-----------|------|-----------|
| R-01 | Builder API for BoundedSessionManager | UC-02 | — | U-01, U-02, T-06, T-07 |
| R-02 | Builder idle_timeout sets SessionConfig::keep_alive | UC-01, SC-01 | V3.3 | U-01 |
| R-03 | Override conflicting SessionConfig::keep_alive with warning | UC-02 | — | U-01 |
| R-04 | Unmanaged keep_alive passthrough when idle_timeout not called | UC-01 | — | U-01 |
| R-05 | One-shot max lifetime task per session | SC-01, AC-01 | V3.3 | T-01, T-03, T-08, I-03 |
| R-06 | Max lifetime task cancelled on early close | SC-01 | V3.3 | T-02, T-08, I-02 |
| R-07 | Tracing event on session created with session_id | UC-03, SC-02 | V7.1 | T-04, I-01 |
| R-08 | Tracing event on session closed with session_id + duration | UC-03, SC-02 | V7.1 | T-01, T-05, I-01 |
| R-09 | Close reason: Evicted, MaxLifetime, or Closed | UC-04 | V7.1 | T-01, T-03, T-05, U-03 |
| R-10 | Existing new() API backward compatible | UC-01 | — | T-06 |
| R-11 | Builder accepts passthrough SessionConfig | UC-02 | — | U-02 |
| R-12 | Builder includes rate_limit configuration | UC-02 | — | T-07 |
| R-13 | memory-mcp --idle-timeout-secs CLI flag + env var | UC-02 | — | M-01, M-03 |
| R-14 | memory-mcp --max-session-lifetime-secs CLI flag + env var | UC-02 | — | M-02, M-03 |
| R-15 | memory-mcp uses builder API | UC-02 | — | M-03 |
