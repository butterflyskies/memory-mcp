<!-- design-meta
status: approved
last-updated: 2026-04-25
phase: 4
-->

# Threat Model — Phase 2 Quality: Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

**Depth:** Lightweight — focused on new attack surface only. Existing flows
(`/mcp`, git remote, OAuth protocol) were covered by the tracing design's
full STRIDE analysis.

## Scope

Two new attack surfaces analyzed:
1. `/readyz` endpoint — unauthenticated HTTP GET, new external surface
2. `DeviceFlowProvider` trait — config injection surface for OAuth parameters

Excluded:
- `VectorStore` trait — internal refactoring, no new network flows or external
  surface. Consumer-visible behavior unchanged.
- Integration test infrastructure — test-only, no production exposure.

## /readyz Endpoint

### STRIDE Analysis

| STRIDE | Threat | Likelihood | Impact | Mitigation |
|--------|--------|------------|--------|------------|
| S | Not applicable. Read-only status endpoint with no identity claims. No actor to impersonate. | — | — | — |
| T | On-path attacker modifies response in transit, causing orchestrator to misroute traffic. | Low | Medium | K8s probes travel over pod network. External probes use TLS at ingress. Deployment concern, not application-level. |
| R | Unauthenticated endpoint leaves no caller identity. | Low | Low | Expected for infrastructure probes — no state change to repudiate. R-20 logs failures. Access logs provide request-level audit. |
| I | Attacker probes `/readyz` to learn which subsystems are running and their health state. Subsystem names confirm architecture (git-backed storage, embedding model, vector index). | Medium | Low | R-18 prohibits internal paths, versions, stack traces, backend details. Subsystem names are generic — confirm architecture but not exploitable details. |
| D | Sustained flooding of `/readyz` consumes CPU and file descriptors. Git repo check acquires a lock on the git2 repository object — high rates could cause lock contention with concurrent MCP operations. | Medium | Medium | R-19 ensures checks are lightweight. **New R-22** adds rate limiting. |
| D | When `--require-remote-sync` is enabled, each probe triggers an outbound connection to the git remote. Flooding amplifies into outbound connection storms, potentially triggering rate limits on the remote. | Medium | Medium | **New R-23** caches remote reachability with a short TTL. |
| E | Not applicable. Read-only endpoint, no interaction with auth/write paths. | Low | Low | — |

## DeviceFlowProvider Config

### STRIDE Analysis

| STRIDE | Threat | Likelihood | Impact | Mitigation |
|--------|--------|------------|--------|------------|
| S | Malicious `DeviceFlowProvider` points URLs at attacker-controlled server mimicking the real OAuth provider. User enters device code on fake site; attacker captures token. | Low | High | Currently not exploitable: `GitHubDeviceFlow` uses compile-time constants, `MockDeviceFlow` is test-only, no user-facing mechanism to inject custom URLs. R-09 (self-validation) and R-10 (HTTPS enforcement) are the foundation. See forward-looking constraint below. |
| T | Compromised build alters compile-time constants to point at malicious OAuth server. | Low | High | Supply-chain attack, out of scope for application-level modeling. Project uses `deny.toml`, SHA-pinned CI actions, signed releases. |
| T | Attacker with test env write access modifies mock server to capture test tokens. | Low | Low | Test tokens have no production value. R-14 ensures no real credentials in CI. |
| R | No structured audit log of which provider was used for login. A silently injected provider would be indistinguishable from legitimate use. | Low | Low | Existing tracing logs `token_source` on resolution. Adding provider identity to the login span would close this gap — minor improvement, not blocking. |
| I | `client_id()` exposes the OAuth app client ID. | Low | Low | Client IDs are inherently public for device flow (RFC 8628 public clients). Not a secret. |
| D | Not applicable to the trait. Network calls using trait URLs are bounded by existing `connect_timeout(10s)`, `timeout(30s)`, and `expires_in.min(1800)` deadline cap. | Low | Low | — |
| E | Custom `DeviceFlowProvider` could request broader OAuth scopes than intended (e.g., `admin:org` instead of `repo`), resulting in elevated token permissions. | Low | High | R-09 requires provider self-validation. `GitHubDeviceFlow` hardcodes `"repo"`. See forward-looking constraint below. |

## New Requirements

| Req ID | Requirement | Source | Security Ref | Test Case |
|--------|-------------|--------|--------------|-----------|
| R-22 | `/readyz` shall be protected against sustained flooding — either via an in-process rate limiter or documented operator responsibility for network-level rate limiting | /readyz DoS | V13.4 | TC-22 (pending) |
| R-23 | When `--require-remote-sync` is enabled, the remote reachability check shall cache its result with a short TTL (5–30 seconds) rather than making a fresh outbound call per request | /readyz amplification | V13.4 | TC-23 (pending) |

## Forward-Looking Constraints

These are not requirements for the current implementation — they constrain future
work that introduces user-configurable OAuth providers.

**Constraint 1: Endpoint allowlisting.** If a user-facing mechanism for specifying
custom OAuth endpoints is added (CLI flags, config file, environment variables),
the accepted `device_code_url` and `access_token_url` values shall be validated
against an allowlist of known providers (e.g., `github.com`, `gitlab.com`) or
require explicit operator opt-in for custom endpoints (e.g.,
`--allow-custom-oauth-endpoint`). Prevents environment variable injection from
silently redirecting the auth flow. (ASVS V14.3)

**Constraint 2: Scope restriction.** When a `DeviceFlowProvider` implementation
allows configurable OAuth scopes, the requested scopes shall be validated against
a maximum allowlist. For GitHub, the maximum allowed scope is `repo`;
`admin:*`, `delete_repo`, and `write:org` shall be rejected. (ASVS V14.2)

## Summary

Overall risk posture: **low**.

The design's existing mitigations (R-18, R-19, R-09, R-10) address the primary
threats. Two new requirements emerged:

1. **Rate limiting on `/readyz`** (R-22) — the highest-priority gap. Each probe
   acquires the git2 repo lock, so sustained flooding could degrade MCP handler
   responsiveness.
2. **Cached remote check** (R-23) — prevents `--require-remote-sync` from
   amplifying probe traffic into outbound connection storms.

The `DeviceFlowProvider` trait introduces minimal current risk because both
implementations are compile-time-fixed or test-only. The spoofing and scope
escalation threats become real only if user-configurable providers are added —
captured as forward-looking constraints rather than immediate requirements.
