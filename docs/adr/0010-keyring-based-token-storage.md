# ADR-0010: Keyring-based token storage via `keyring` crate

## Status
Accepted

## Context
The server needs a GitHub token for git push/pull. The current resolution chain
(env var, plaintext file) works but is weaker than what users expect on desktop
Linux, where KWallet or GNOME Keyring manage secrets via D-Bus Secret Service.

Three options were evaluated:
1. **Shell out to `gh auth token`** — zero new deps, but creates a hard dependency
   on the GitHub CLI being installed and authenticated.
2. **`keyring` crate** (v3.6, `sync-secret-service` feature) — cross-platform Rust
   library that talks to KWallet/GNOME Keyring/macOS Keychain natively. Sync API,
   graceful `NoStorageAccess` error when no backend is available.
3. **Direct `secret-service` + `zbus`** — lower-level D-Bus bindings. More control
   but Linux-only and heavier dependency surface.

## Decision
Use the `keyring` crate with `sync-secret-service` feature on Linux. Insert it as
step 3 in the token resolution chain: env var → file → keyring → error. The env var
and file fallbacks remain for headless/CI environments where no keyring daemon runs.

Service name: `memory-mcp`, user: `github-token`.

## Consequences
- Desktop users get automatic integration with their system secret store.
- Headless/CI deployments are unaffected — keyring errors are caught and the chain
  falls through to the existing fallbacks.
- Adds one new dependency (`keyring`) with its transitive D-Bus deps on Linux.
- A future MCP tool (`auth_store_token`) can call `entry.set_password()` to let
  agents store tokens without touching the filesystem.
