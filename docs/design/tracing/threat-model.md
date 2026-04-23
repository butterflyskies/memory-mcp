<!-- design-meta
status: draft
last-updated: 2026-04-23
phase: 4
-->

# Threat Model: Tracing Scaffold (#52)

## Scope

Lightweight review focused on the trace output boundary — the one area
where this work introduces new risk. Full STRIDE analysis is deferred to
#115 (auth framework), where the real trust boundary complexity arrives.
That deferral is documented on #115.

## Trust boundaries

1. **AI agent → MCP server** (HTTP request boundary) — not changed by this work
2. **MCP server → trace output** (stderr + OTLP) — **new/expanded by this work**
3. **MCP server → git remote** (HTTPS + PAT) — not changed by this work

## Threat analysis: trace output boundary

| STRIDE | Threat | Likelihood | Impact | Mitigation |
|--------|--------|------------|--------|------------|
| I | Auth tokens leak into span fields via `auth.resolve` instrumentation | Medium | High (credential compromise) | R-16: redact at source. Auth spans log `token_source`, never the value. |
| I | Memory content leaks into span fields | Medium | Medium (content may be sensitive) | R-17: `content_size` only, never text. Phase 5 will revisit with tiered output. |
| I | Query text in `recall` spans exposes user search intent | Medium | Medium (queries may contain sensitive phrasing) | Excluded from scaffold spans. Phase 5 will add to audit channel with restricted access. See architecture.md "Design Tension" section. |
| I | Git credentials leak via remote URL in span fields | Low | High (repo access compromise) | R-18: all URLs pass through existing `redact_url`. |
| I | OTLP export sends spans to unintended/compromised collector | Low | High (all trace data exfiltrated) | OTLP is opt-in (feature flag). Endpoint is operator-configured. Operational concern, not code mitigation. |
| D | High-volume operations flood trace output, filling disk | Low | Low (bounded by existing rate limiting) | R-20: appropriate default levels. `session_rate_limit` bounds request volume. |
| T | Trace data modified in transit to OTLP collector | Low | Low (observational data, not authoritative) | TLS on OTLP transport (standard configuration). |

## Findings

1. **No new requirements needed.** R-16, R-17, R-18, R-20, and R-21 cover
   the identified threats. The audit content tension (query text, memory
   content) is documented in `architecture.md` and deferred to Phase 5.

2. **Trace output is sensitive data.** Once the scaffold is in place, trace
   output contains memory names, scopes, session IDs, and operational
   patterns. Operators should treat trace output with appropriate access
   controls. This is an operational guidance concern, not a code change.

3. **Full STRIDE deferred to #115.** The auth framework introduces spoofing,
   elevation of privilege, and access control threats that don't exist in
   the current single-user model. Documented on the issue.

## Deferred analysis

| Concern | Deferred to | Rationale |
|---------|-------------|-----------|
| Full STRIDE on all trust boundaries | #115 (auth framework) | Multi-user auth introduces the real trust boundary complexity |
| Audit content vs. privacy resolution | Phase 5 (#110, #111, #112) | Requires tiered output architecture — operational traces vs. audit log |
| Trace output access controls | Phase 6 (#115, #118) | Meaningful only in multi-user deployment |
