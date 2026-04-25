<!-- design-meta
status: approved
last-updated: 2026-04-25
phase: 2
-->

# Requirements — Phase 2 Quality: Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

## Use Cases

### Actors

| Actor | Description |
|-------|-------------|
| MCP Client | AI agent or tool calling memory operations via MCP |
| Operator | Person or system deploying/monitoring memory-mcp |
| Developer | Contributor writing or running tests against the codebase |
| Attacker | Malicious actor with network access to the HTTP endpoint |

### Use case table

| ID | Actor | Use Case | Type | Priority |
|----|-------|----------|------|----------|
| UC-01 | MCP Client | Store a memory and retrieve it by semantic similarity | Normal | Must |
| UC-02 | MCP Client | Remove a memory and have it disappear from search results | Normal | Must |
| UC-03 | MCP Client | Get correct results regardless of which backend implements the index | Normal | Must |
| UC-04 | MCP Client | Receive a meaningful error when a vector operation fails | Normal | Must |
| UC-05 | Operator | Query /readyz to determine if the service can handle requests | Normal | Must |
| UC-06 | Operator | See which specific subsystem is unhealthy when /readyz returns 503 | Normal | Must |
| UC-07 | Operator | Use /readyz as a k8s readiness probe (HTTP GET, status code contract) | Normal | Must |
| UC-08 | Developer | Run integration tests for auth login against a mock OAuth server | Normal | Must |
| UC-09 | Developer | Run integration tests for auth status reporting | Normal | Must |
| UC-10 | Developer | Run integration tests for MEMORY_MCP_BIND override | Normal | Must |
| UC-11 | Developer | Inject index backend failures to test rollback correctness | Normal | Must |
| UC-12 | Developer | Substitute the index backend for fast, deterministic tests | Normal | Should |
| UC-13 | Operator | Configure an alternative device flow provider (e.g., GitLab) for auth | Normal | Should |
| AC-01 | Attacker | Probe /readyz to fingerprint subsystem versions or internal state | Abuse | Must-mitigate |
| AC-02 | Attacker | Flood /readyz to cause resource exhaustion via expensive health checks | Abuse | Must-mitigate |
| AC-03 | Attacker | Supply a crafted device flow provider config to redirect auth flow | Abuse | Must-mitigate |
| SC-01 | System | Health checks must not expose internal paths, versions, or error details | Security | Must |
| SC-02 | System | Health checks must be lightweight — no full index scans or embedding runs | Security | Must |
| SC-03 | System | OAuth device flow provider config must validate parameters before use | Security | Should |

## Requirements

### Vector storage trait (#94)

| Req ID | Requirement | Source UC | Security Ref | Test Case |
|--------|-------------|-----------|--------------|-----------|
| R-01 | System shall define a public `VectorStore` trait with operations: add (name, scope, vector), remove (name, scope), search (query, scope filter, limit), save, load, health check | UC-01, UC-02, UC-03 | V1.1 | TC-01 (pending) |
| R-02 | The usearch-backed implementation shall preserve all existing behavior — add, remove, search, scoped retrieval, save/load round-trip, upsert semantics | UC-01, UC-02, UC-03 | — | TC-02 (pending) |
| R-03 | The usearch implementation shall use a private internal trait for raw index operations to allow failure injection | UC-11 | V1.1 | TC-03 (pending) |
| R-04 | When the all-index insert fails after a scope-index insert succeeds, the scope insert shall be rolled back, restoring the index to its pre-call state | UC-04, UC-11 | V7.1 | TC-04 (pending) |
| R-05 | Errors from the vector storage trait shall propagate as typed errors without exposing backend-specific details to callers | UC-04 | V7.2, A.8.11 | TC-05 (pending) |
| R-06 | The `VectorStore` trait shall include a readiness method that reports whether the backend can serve queries | UC-05, UC-12 | V13.1 | TC-06 (pending) |
| R-07 | Consumer code (server, MCP handlers) shall program against the `VectorStore` trait, not concrete usearch types | UC-03 | V1.1 | TC-07 (pending) |

### OAuth device flow provider abstraction (#145)

| Req ID | Requirement | Source UC | Security Ref | Test Case |
|--------|-------------|-----------|--------------|-----------|
| R-08 | OAuth device flow parameters (client ID, device code URL, access token URL, scopes) shall be sourced from a `DeviceFlowProvider` trait, with GitHub as the default implementation | UC-08, UC-13, AC-03 | V14.2 | TC-08 (pending) |
| R-09 | Each provider implementation shall validate its own parameters before use in the auth flow (e.g., client ID format, URL scheme) | AC-03 | V5.1, V14.3 | TC-09 (pending) |
| R-10 | OAuth device flow endpoint URLs shall only permit HTTPS scheme (except localhost for development/testing) | AC-03 | V5.1, V14.3 | TC-10 (pending) |

### Integration tests (#146)

| Req ID | Requirement | Source UC | Security Ref | Test Case |
|--------|-------------|-----------|--------------|-----------|
| R-11 | Integration tests shall exercise `auth login` against a mock OAuth server that implements the device flow protocol | UC-08 | — | TC-11 (pending) |
| R-12 | Integration tests shall verify `auth status` reports correct token source and provenance | UC-09 | — | TC-12 (pending) |
| R-13 | Integration tests shall verify the server binds to the address specified by `MEMORY_MCP_BIND` | UC-10 | — | TC-13 (pending) |
| R-14 | Integration tests shall run in CI without real OAuth credentials from any provider | UC-08, UC-11 | — | TC-14 (pending) |

### Health endpoint (#164)

| Req ID | Requirement | Source UC | Security Ref | Test Case |
|--------|-------------|-----------|--------------|-----------|
| R-15 | `/readyz` shall return 200 with a JSON body when all subsystems are healthy | UC-05, UC-07 | V13.1 | TC-15 (pending) |
| R-16 | `/readyz` shall return 503 with a JSON body identifying which subsystem(s) failed when any check fails | UC-06, UC-07 | V13.1, A.8.16 | TC-16 (pending) |
| R-17 | `/readyz` shall check: git repo accessible and valid (must work for empty repos with no commits), embedding-index dimensional consistency (`embedding.dimensions() == index.dimensions()`), vector index ready (via trait readiness method) | UC-05, UC-06 | A.8.16 | TC-17 (pending) |
| R-18 | `/readyz` response shall not include internal file paths, version strings, error stack traces, or backend-specific details | AC-01, SC-01 | V7.4, A.8.11 | TC-18 (pending) |
| R-19 | `/readyz` health checks shall be lightweight — status queries only, no embedding generation, no index scans | AC-02, SC-02 | V13.4 | TC-19 (pending) |
| R-20 | `/readyz` failures shall be logged at warn level with subsystem detail via the existing tracing infrastructure | UC-06 | V7.1, A.8.15 | TC-20 (pending) |
| R-21 | Remote sync unavailability shall not affect readiness by default; an opt-in flag (e.g., `--require-remote-sync`) shall make remote reachability a readiness condition | UC-05, UC-07 | V14.2 | TC-21 (pending) |
| R-22 | `/readyz` shall be protected against sustained flooding — either via an in-process rate limiter or documented operator responsibility for network-level rate limiting | AC-02 | V13.4 | TC-22 (pending) |
| R-23 | When `--require-remote-sync` is enabled, the remote reachability check shall cache its result with a short TTL (5–30 seconds) rather than making a fresh outbound call per request | AC-02 | V13.4 | TC-23 (pending) |

## ASVS & ISO 27001 Review

### Applicable categories

| Framework | Category | Relevance |
|-----------|----------|-----------|
| ASVS V1 | Architecture, design, threat modeling | The public trait boundary is a trust boundary between consumers and backend internals |
| ASVS V5 | Validation, sanitization, encoding | Provider config input validation, /readyz response sanitization |
| ASVS V7 | Error handling, logging | Error propagation through the trait, health check failure reporting and logging |
| ASVS V13 | API and web services | /readyz is a new HTTP endpoint with a status code contract |
| ASVS V14 | Configuration | OAuth provider config extraction and validation |
| ISO A.8.11 | Data masking | /readyz must not leak internal paths or state details |
| ISO A.8.15 | Logging | Health check failures logged for operational awareness |
| ISO A.8.16 | Monitoring activities | /readyz is the monitoring surface itself |

### Not applicable

| Framework | Category | Rationale |
|-----------|----------|-----------|
| ASVS V2 | Authentication | Not changing auth mechanics — extracting config and adding provider abstraction |
| ASVS V3 | Session management | No session changes |
| ASVS V6 | Stored cryptography | No crypto changes |
| ASVS V8 | Data protection | No new sensitive data handling |
| ASVS V9 | Communication | No transport changes |
| ASVS V10 | Malicious code / supply chain | No new dependencies |
| ASVS V11 | Business logic | No business logic changes |
| ASVS V12 | Files and resources | No file upload or resource handling changes |
| ISO A.5.33 | Protection of records | No new audit/log record types beyond existing tracing |
| ISO A.8.10 | Information deletion | No data lifecycle changes |
| ISO A.8.12 | Data leakage prevention | Covered by A.8.11 for /readyz |
| ISO A.8.17 | Clock synchronization | No timestamp changes |

## Design Notes

- **Provider abstraction scope:** The `auth::oauth::DeviceFlowProvider` trait
  covers RFC 8628 device authorization grant specifically — not all OAuth grant
  types. The `auth::oauth` module namespace provides the OAuth context, so the
  trait name doesn't need an `OAuth` prefix. Device flow is the right choice for
  CLI tools, and both GitHub and GitLab support it. If a future provider needs a
  different grant type, the path is a higher-level `AuthFlow` trait with
  `DeviceFlowProvider` as one strategy, alongside sibling modules under `auth/`.
  See [ADR-0024](../../adr/0024-oauth-device-flow-provider-trait.md) for full
  analysis. We are implementing the GitHub provider now; GitLab and others are
  future implementations.
- **R-09 validation:** GitHub OAuth client IDs follow a known format (`Iv1.` prefix +
  hex characters). GitLab uses a different format. Provider-specific validation belongs
  in each provider's implementation, not in a universal rule.
- **R-06 as the bridge:** The trait's readiness method is how /readyz checks vector
  index health without reaching into usearch internals. This is the key connection
  between #94 and #164.
- **Rollback testing at two levels:** R-03 enables private failure injection to test
  the usearch implementation's internal rollback mechanics. R-05 enables public
  trait-level tests to verify error behavior regardless of backend — both are needed
  and test different things.
