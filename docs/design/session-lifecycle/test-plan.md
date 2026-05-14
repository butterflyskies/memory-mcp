<!-- design-meta
status: approved
last-updated: 2026-05-14
phase: 5
-->

# Test Plan: Session Lifecycle Observability (#114)

Integration tests use `tokio::time::pause()` for deterministic time control.
Tracing assertions use a capturing subscriber layer (assert on field
presence/values, not log formatting).

## mcp-session tests

### Unit tests

| Test ID | Req | Description |
|---------|-----|-------------|
| U-01 | R-02, R-03, R-04 | Builder idle_timeout/passthrough resolution: (a) idle_timeout set → keep_alive = idle_timeout, (b) both set → idle_timeout wins + warn logged, (c) only passthrough → passthrough value used, (d) neither → SessionConfig default |
| U-02 | R-11 | Builder passthrough: `channel_capacity`, `sse_retry`, `completed_cache_ttl`, `init_timeout` all flow through to inner SessionConfig |
| U-03 | R-09 | `CloseReason` implements Debug, Clone, Copy, PartialEq, Eq |

### Integration tests

| Test ID | Req | Description |
|---------|-----|-------------|
| T-01 | R-05, R-08, R-09 | Max lifetime closes session: create with `max_lifetime=100ms`, advance 100ms, assert `has_session` returns false. Tracing event has `reason=MaxLifetime` and `duration_secs` in expected range |
| T-02 | R-06 | Early close cancels max lifetime task: create with `max_lifetime=1s`, close immediately, advance 1.5s, assert no panic or double-close |
| T-03 | R-05, R-06, R-09 | Eviction + max lifetime: `max_sessions=1`, `max_lifetime=5s`. Create A, create B (evicts A with reason=Evicted). Advance 5s, B closes with reason=MaxLifetime |
| T-04 | R-07 | Session creation emits tracing event with `session_id` field |
| T-05 | R-08, R-09 | Session close emits tracing event with `session_id`, `duration_secs`, and `reason` |
| T-06 | R-10 | Backward compat: `BoundedSessionManager::new(config, max)` works, no lifecycle tracking or max lifetime tasks |
| T-07 | R-12 | Builder with only `.rate_limit()` — rate limiting works, no lifecycle tasks spawned |
| T-08 | R-05, R-06 | Multiple sessions with max lifetime: create 5 sessions with `max_lifetime=100ms`, advance time, verify all 5 close. No leaked tasks or entries. |

### Invariant tests

These test structural invariants across operation sequences. Not property
tests in the QuickCheck sense — they're deterministic scenarios designed
to exercise the invariant, not random generation.

| Test ID | Req | Invariant | Scenario |
|---------|-----|-----------|----------|
| I-01 | R-07, R-08 | Lifecycle map has exactly one entry per live session | Create 5, close 2, assert map len = 3. Close remaining 3, assert map empty. |
| I-02 | R-06 | No abort handles leak after all sessions close | Create 3 with max_lifetime. Close all. Advance past max_lifetime. Assert no panics, no task activity. |
| I-03 | R-05, R-09 | Eviction + timeout interactions don't corrupt state | `max_sessions=2`. Create A, B, C (evicts A), D (evicts B). Advance past max_lifetime. Assert C and D close with MaxLifetime. Lifecycle map empty. |

## memory-mcp tests

| Test ID | Req | Description |
|---------|-----|-------------|
| M-01 | R-13 | `--idle-timeout-secs 300` parses correctly. Default is 14400. `MEMORY_MCP_IDLE_TIMEOUT_SECS` env var works. |
| M-02 | R-14 | `--max-session-lifetime-secs 600` parses correctly. Default is 0 (disabled). Env var works. |
| M-03 | R-15 | Server with `--idle-timeout-secs 300 --max-session-lifetime-secs 3600` starts and uses builder API |

## SRTM (final)

| Req ID | Requirement | Test Cases |
|--------|-------------|------------|
| R-01 | Builder API | U-01, U-02, T-06, T-07 |
| R-02 | idle_timeout sets keep_alive | U-01 |
| R-03 | Override conflicting keep_alive | U-01 |
| R-04 | Passthrough when idle_timeout not called | U-01 |
| R-05 | One-shot max lifetime task | T-01, T-03, T-08, I-03 |
| R-06 | AbortHandle cancelled on early close | T-02, T-08, I-02 |
| R-07 | Tracing: session created | T-04, I-01 |
| R-08 | Tracing: session closed with duration | T-01, T-05, I-01 |
| R-09 | Close reason enum | T-01, T-03, T-05, U-03 |
| R-10 | Existing new() backward compat | T-06 |
| R-11 | Builder passthrough SessionConfig | U-02 |
| R-12 | Builder includes rate_limit | T-07 |
| R-13 | memory-mcp --idle-timeout-secs | M-01, M-03 |
| R-14 | memory-mcp --max-session-lifetime-secs | M-02, M-03 |
| R-15 | memory-mcp uses builder API | M-03 |
