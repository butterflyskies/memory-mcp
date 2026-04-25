<!-- design-meta
status: approved
last-updated: 2026-04-25
phase: 5
-->

# Test Plan — Phase 2 Quality: Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

## Test Strategy

Tests are organized in three layers:

1. **Unit tests** — in-module `#[cfg(test)]` blocks, fast, no I/O
2. **Integration tests** — `tests/` directory, may start servers or use tempdir,
   `#[tokio::test]`
3. **Compile-time checks** — verified by the type system (trait bounds, visibility)

All tests must pass in CI without external services (no real GitHub OAuth, no
real git remote, no network access).

## Test Cases

### Vector storage trait (#94)

| Test ID | Req | Type | Description |
|---------|-----|------|-------------|
| TC-01 | R-01 | Compile-time | `VectorStore` trait is public, object-safe, and defines all required methods (add, remove, search, save, load, is_ready, find_by_name, dimensions, commit_sha/set_commit_sha). Verified by compiling `Box<dyn VectorStore>` in AppState. |
| TC-02a | R-02 | Unit | `UsearchStore::add` followed by `search` returns the added entry with correct name and scope |
| TC-02b | R-02 | Unit | `UsearchStore::remove` makes the entry unreachable via `search` and `find_by_name` |
| TC-02c | R-02 | Unit | `UsearchStore::add` with an existing name replaces the old entry (upsert semantics) |
| TC-02d | R-02 | Unit | `UsearchStore::search` with scope filter returns only entries matching that scope plus global |
| TC-02e | R-02 | Integration | `UsearchStore::save` then `UsearchStore::load` round-trips all entries, names, scopes, and commit SHA |
| TC-03 | R-03 | Unit | `UsearchStore` can be constructed with a `FailingRawIndex` that implements the private `RawIndex` trait. The test compiles and the failing backend is injectable. |
| TC-04a | R-04 | Unit | With `FailingRawIndex` configured to fail on the all-index insert: `UsearchStore::add` returns an error, and the scope index is unchanged (entry count before == entry count after) |
| TC-04b | R-04 | Unit | With `FailingRawIndex` configured to fail on the all-index insert: a previously existing entry with the same name is not corrupted by the failed upsert |
| TC-05a | R-05 | Unit | When `UsearchStore::add` fails, the returned error is a `MemoryError` variant, not a usearch-specific type |
| TC-05b | R-05 | Unit | Error `Display` output does not contain usearch crate paths, internal key IDs, or file system paths |
| TC-05c | R-05 | Unit | `InMemoryStore` returns the same `MemoryError` variants for equivalent failure conditions |
| TC-06a | R-06 | Unit | `UsearchStore::is_ready()` returns ready after successful construction and load |
| TC-06b | R-06 | Unit | `InMemoryStore::is_ready()` returns ready after construction |
| TC-07 | R-07 | Compile-time | `AppState.index` is `Box<dyn VectorStore>`. MCP handler code in `server.rs` references only `VectorStore` trait methods — no direct import of `UsearchStore`, `ScopedIndex`, or `VectorIndex`. Verified by grep + compilation. |

### Device flow provider abstraction (#145)

| Test ID | Req | Type | Description |
|---------|-----|------|-------------|
| TC-08a | R-08 | Unit | `GitHubDeviceFlow` implements `DeviceFlowProvider` and returns expected client ID, URLs, and scopes |
| TC-08b | R-08 | Unit | `device_flow_login()` accepts `&dyn DeviceFlowProvider` — compiles with both `GitHubDeviceFlow` and `MockDeviceFlow` |
| TC-09a | R-09 | Unit | `GitHubDeviceFlow::validate()` returns `Ok(())` (its constants are valid) |
| TC-09b | R-09 | Unit | A `DeviceFlowProvider` with an empty client ID fails `validate()` |
| TC-09c | R-09 | Unit | A `DeviceFlowProvider` with a malformed client ID (wrong format for the provider) fails `validate()` |
| TC-10a | R-10 | Unit | A `DeviceFlowProvider` with `http://` device code URL fails `validate()` |
| TC-10b | R-10 | Unit | A `DeviceFlowProvider` with `http://localhost` device code URL passes `validate()` (dev exception) |
| TC-10c | R-10 | Unit | A `DeviceFlowProvider` with `https://` URLs passes `validate()` |

### Integration tests (#146)

| Test ID | Req | Type | Description |
|---------|-----|------|-------------|
| TC-11a | R-11 | Integration | Start a mock OAuth server on an ephemeral port. Create a `MockDeviceFlow` pointing at it. Call `device_flow_login()` with the mock provider. The mock server receives the device code request with the mock client ID and scopes, returns a device code response, then returns an access token on the next poll. Verify the token is stored. |
| TC-11b | R-11 | Integration | Mock OAuth server returns `authorization_pending` for 2 polls, then returns the token. Verify `device_flow_login()` retries and succeeds. |
| TC-11c | R-11 | Integration | Mock OAuth server returns `access_denied`. Verify `device_flow_login()` returns an appropriate error. |
| TC-12a | R-12 | Integration | Set `MEMORY_MCP_GITHUB_TOKEN` env var, call `auth status` logic, verify output reports token source as environment variable |
| TC-12b | R-12 | Integration | Write a token to the token file path, call `auth status` logic, verify output reports token source as file |
| TC-13 | R-13 | Integration | Set `MEMORY_MCP_BIND=127.0.0.1:<ephemeral>`, start the server, verify it listens on the specified address by connecting to it |
| TC-14 | R-14 | Compile-time + CI | All integration tests in this group pass in CI without `MEMORY_MCP_GITHUB_TOKEN` set and without network access to github.com. Verified by CI environment configuration. |

### Health endpoint (#164)

| Test ID | Req | Type | Description |
|---------|-----|------|-------------|
| TC-15 | R-15 | Integration | Start server with healthy subsystems. `GET /readyz` returns 200 with JSON body containing `"status":"ready"` and per-subsystem `"status":"up"` |
| TC-16a | R-16 | Integration | Start server with a `VectorStore` that reports not-ready. `GET /readyz` returns 503 with JSON body containing `"status":"not_ready"` and the vector index check showing `"status":"down"` with a reason string |
| TC-16b | R-16 | Integration | Start server with an inaccessible git repo. `GET /readyz` returns 503 with the git repo check showing `"status":"down"` |
| TC-16c | R-16 | Integration | Start server with mismatched embedding/index dimensions. `GET /readyz` returns 503 with embedding check showing `"status":"down"` |
| TC-17a | R-17 | Integration | Start server with a freshly initialized empty git repo (no commits). `GET /readyz` returns 200 — the git repo check passes for empty repos |
| TC-17b | R-17 | Integration | Start server where `embedding.dimensions()` returns 384 and `index.dimensions()` returns 384. `GET /readyz` shows embedding check as `"up"` |
| TC-17c | R-17 | Integration | Start server where `embedding.dimensions()` returns 384 and `index.dimensions()` returns 768. `GET /readyz` shows embedding check as `"down"` with dimensional mismatch reason |
| TC-18a | R-18 | Integration | `GET /readyz` response body (both 200 and 503 cases) does not contain any absolute file path patterns (`/home/`, `/var/`, `/tmp/`) |
| TC-18b | R-18 | Integration | `GET /readyz` 503 response body reason fields match a known allowlist of strings (no free-form error messages) |
| TC-19 | R-19 | Unit | The readyz handler calls only `is_accessible()` on repo, `dimensions()` on embedding and index, and `is_ready()` on VectorStore. Verified by inspection + mock implementations that panic if embed() or search() are called during a readyz check. |
| TC-20 | R-20 | Integration | Start server with a failing subsystem. `GET /readyz` returns 503. Captured tracing events include a warn-level event naming the failed subsystem. Uses in-memory span capture from the tracing test infrastructure. |
| TC-21a | R-21 | Integration | Start server without `--require-remote-sync`, with no git remote configured. `GET /readyz` returns 200. |
| TC-21b | R-21 | Integration | Start server with `--require-remote-sync`, with no reachable git remote. `GET /readyz` returns 503 with remote sync check showing `"down"`. |
| TC-22 | R-22 | Integration | Send 100 rapid `GET /readyz` requests. If in-process rate limiting is chosen: verify that requests beyond the limit receive 429 or are dropped. If operator-documented: verify the documentation exists and the endpoint functions normally (test serves as a baseline performance assertion). |
| TC-23 | R-23 | Integration | Start server with `--require-remote-sync` and a mock git remote. Send two `GET /readyz` requests within 1 second. Verify the mock remote received at most one connection attempt (second request used cached result). |

## Test Infrastructure

### New test utilities needed

- **`InMemoryStore`** — `VectorStore` implementation backed by `HashMap`. Supports
  configurable `is_ready()` return value and optional dimensional override for
  testing mismatch scenarios.
- **`FailingRawIndex`** — private `RawIndex` implementation that errors on
  configurable operations (e.g., "fail on the Nth add to the all-index").
- **`MockDeviceFlow`** — `DeviceFlowProvider` implementation with configurable
  URLs pointing at a test server.
- **Mock OAuth server** — minimal Axum app implementing `/device/code` and
  `/access_token` endpoints with configurable response sequences (pending,
  denied, success).
- **`MockEmbeddingBackend`** — `EmbeddingBackend` implementation with configurable
  `dimensions()` return value and `embed()`/`embed_one()` that panic if called
  during health checks (TC-19).

### Existing test infrastructure reused

- **In-memory span capture** from tracing scaffold (#52) — used for TC-20 to verify
  warn-level log events on readyz failure.
- **`tempfile::tempdir()`** — used for git repo and index persistence tests.
- **`AuthProvider::with_token()`** — existing test constructor for auth.

## Traceability Summary

| Req | Test Cases | Coverage |
|-----|------------|----------|
| R-01 | TC-01 | Compile-time trait verification |
| R-02 | TC-02a–e | Behavioral parity across 5 operations |
| R-03 | TC-03 | Failure injection compiles |
| R-04 | TC-04a–b | Rollback on failure + no corruption |
| R-05 | TC-05a–c | Typed errors, no backend leakage, consistency across impls |
| R-06 | TC-06a–b | Readiness for both implementations |
| R-07 | TC-07 | No concrete type imports in consumer code |
| R-08 | TC-08a–b | Trait implementation + polymorphic dispatch |
| R-09 | TC-09a–c | Valid, empty, and malformed client ID |
| R-10 | TC-10a–c | HTTP rejected, localhost exception, HTTPS accepted |
| R-11 | TC-11a–c | Happy path, retry, and denial |
| R-12 | TC-12a–b | Env var and file token sources |
| R-13 | TC-13 | Bind address override |
| R-14 | TC-14 | CI-safe (no real credentials) |
| R-15 | TC-15 | Healthy response |
| R-16 | TC-16a–c | Per-subsystem failure reporting |
| R-17 | TC-17a–c | Empty repo, matching dims, mismatched dims |
| R-18 | TC-18a–b | No path leakage, allowlisted reasons |
| R-19 | TC-19 | No heavy operations during health check |
| R-20 | TC-20 | Warn-level tracing on failure |
| R-21 | TC-21a–b | Default off, opt-in on |
| R-22 | TC-22 | Rate limiting or baseline |
| R-23 | TC-23 | Cached remote check |
