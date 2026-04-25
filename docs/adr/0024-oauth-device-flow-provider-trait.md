# ADR-0024: `auth::oauth::DeviceFlowProvider` trait for multi-provider device flow auth

## Status
Proposed

## Context
ADR-0012 introduced the OAuth device flow for token acquisition, with the GitHub
client ID, device code URL, and access token URL hardcoded as constants in
`auth.rs`. This works for GitHub-only use, but:

1. **Integration tests** cannot exercise the device flow without hitting real
   GitHub endpoints — there is no way to point the flow at a mock server.
2. **Alternative providers** (GitLab, Gitea) also support RFC 8628 device flow
   but use different client IDs, endpoint URLs, and scope formats. Supporting
   them would require rewriting `device_flow_login()`.
3. The hardcoded constants mix configuration with logic, making the auth module
   harder to test and extend.

We considered three abstraction levels:

- **`OAuthProvider`** (general OAuth trait): rejected because the trait methods
  (`device_code_url`, `access_token_url`) are specific to the device flow grant
  type. Providers that only support authorization code flow or API keys wouldn't
  fit this shape, and the name would imply generality the trait doesn't have.
- **`auth::oauth::DeviceFlowProvider`** (RFC 8628 trait): chosen because it
  accurately describes the scope — any provider implementing RFC 8628 device
  authorization grant can implement this trait. GitHub and GitLab both qualify.
  The `auth::oauth` module namespace provides the OAuth context, so the trait
  name itself doesn't need the `OAuth` prefix.
- **`AuthFlow`** (high-level auth strategy trait): deferred. If a future provider
  needs a fundamentally different grant type, the right move is a higher-level
  trait where `DeviceFlowProvider` becomes one implementation strategy.
  This refactor is mechanical when there's a real second flow to design against.

## Decision
Introduce an `auth::oauth` module with a `DeviceFlowProvider` trait covering
the RFC 8628 device authorization grant:

```rust
// src/auth/oauth/mod.rs
trait DeviceFlowProvider: Send + Sync {
    fn client_id(&self) -> &str;
    fn device_code_url(&self) -> &str;
    fn access_token_url(&self) -> &str;
    fn scopes(&self) -> &[&str];
    fn validate(&self) -> Result<(), MemoryError>;
}
```

Module structure:

```
auth/
  oauth/
    mod.rs        — DeviceFlowProvider trait, device_flow_login()
    github.rs     — GitHubDeviceFlow (zero-sized, compile-time constants)
  mod.rs          — AuthProvider, token resolution, store backends
```

`device_flow_login()` takes `&dyn DeviceFlowProvider` instead of importing
constants directly. The current constants become the `GitHubDeviceFlow`
implementation. A `MockDeviceFlow` implementation points at an in-process test
server for integration tests.

Each provider validates its own parameters (`validate()`) — GitHub checks its
`Iv1.` client ID format, GitLab would check its own. Endpoint URLs must use
HTTPS (except localhost for testing).

Future auth strategies that aren't OAuth device flow (authorization code,
API keys) would be sibling modules under `auth/`, not forced into the
`auth::oauth` module:

```
auth/
  oauth/          — DeviceFlowProvider, AuthCodeProvider (future)
  api_key/        — ApiKeyProvider (future, non-OAuth)
  mod.rs          — AuthProvider
```

## Consequences
- `device_flow_login()` becomes testable against a mock OAuth server
- GitLab and Gitea support is unblocked — implement the trait, no flow changes
- Scopes are provider-defined (`"repo"` for GitHub, different for GitLab)
- The `DeviceCodeResponse` and `AccessTokenResponse` structs remain shared —
  RFC 8628 standardises the response format across providers
- If a non-device-flow provider is needed later, introduce a higher-level
  `AuthFlow` trait — `DeviceFlowProvider` becomes one strategy, alongside
  `AuthCodeProvider` or `ApiKeyProvider`, each in their own module under `auth/`
- Supersedes the hardcoded constants from ADR-0012 but does not invalidate
  ADR-0012's other decisions (keyring storage, stdout backend, etc.)
