# mcp-session

Bounded session management for [MCP](https://modelcontextprotocol.io) servers built on [rmcp](https://crates.io/crates/rmcp).

MCP's Streamable HTTP transport creates server-side sessions on each client `initialize` request. Without bounds, sessions accumulate indefinitely — a resource exhaustion vector. This crate wraps rmcp's `LocalSessionManager` with production-hardened session lifecycle controls.

## Features

- **Max concurrent sessions** — FIFO eviction of the oldest session when the configured limit is reached
- **Rate limiting** — sliding-window counter rejects bursts of session creation (prevents session-flood DoS)
- **Idle timeout** — pass-through configuration of rmcp's `keep_alive` timer
- **Zero dead allocation** — rate limiter state is only allocated when rate limiting is enabled

## Usage

```rust
use std::sync::Arc;
use mcp_session::{BoundedSessionManager, SessionConfig};

let manager = Arc::new(
    BoundedSessionManager::new(
        SessionConfig {
            keep_alive: Some(std::time::Duration::from_secs(4 * 60 * 60)),
            ..Default::default()
        },
        100, // max concurrent sessions
    )
    .with_rate_limit(10, std::time::Duration::from_secs(60)), // 10 new sessions per minute
);

// Pass to StreamableHttpService::new(factory, manager, config)
```

## Design

`BoundedSessionManager` implements rmcp's `SessionManager` trait by delegating to `LocalSessionManager` for session storage and lifecycle, adding:

1. **Capacity check** using `inner.sessions` (the authoritative live count) — expired sessions don't consume capacity slots
2. **FIFO eviction** via a `VecDeque<SessionId>` tracking creation order
3. **Rate limiting** via an optional `RateLimiter` with sliding-window token rollback on creation failure
4. **Split critical sections** — no locks held across async operations

See [ADR-0020](https://github.com/butterflyskies/memory-mcp/blob/main/docs/adr/0020-bounded-session-management.md) for the architectural decision record.

## Concurrency note

Under concurrent session creation, the live count may transiently exceed `max_sessions` by at most the number of concurrent callers. The limit is best-effort under contention. See [Discussion #83](https://github.com/butterflyskies/memory-mcp/discussions/83) for the design tradeoff.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
