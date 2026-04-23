<!-- design-meta
status: draft
last-updated: 2026-04-23
phase: 2
-->

# Requirements: Tracing Scaffold (#52)

## Use Cases

| ID | Actor | Use Case | Type | Priority |
|----|-------|----------|------|----------|
| UC-01 | Operator | Trace a slow tool call to identify which subsystem (embedding, index, repo) is the bottleneck | Normal | Must |
| UC-02 | Operator | Monitor startup to see which subsystem initialization is slow (model load, index load, repo open) | Normal | Must |
| UC-03 | Operator | Filter traces by MCP session ID to isolate a specific client's activity | Normal | Must |
| UC-04 | Operator | Export traces via OTLP to Honeycomb/Jaeger/Tempo for analysis and alerting | Normal | Must |
| UC-05 | Operator | Adjust trace verbosity at runtime via `RUST_LOG` without restarting the server | Normal | Should |
| UC-06 | Developer | Add a new subsystem method and instrument it by following existing conventions | Normal | Must |
| UC-07 | Developer | Read trace output during local development to understand request flow | Normal | Must |
| AC-01 | Attacker | Read trace output to extract auth tokens, memory content, or git credentials | Abuse | Must-mitigate |
| AC-02 | Attacker | Trigger operations that produce excessive trace output to fill disk or flood a log aggregator | Abuse | Should-mitigate |
| SC-01 | System | Redact sensitive fields (tokens, credentials, full memory content) in all trace output regardless of log level | Security | Must |
| SC-02 | System | Ensure auth token resolution path is logged at appropriate level (which source was tried/succeeded, never the token value) | Security | Must |

## ASVS Categories Reviewed

| Category | Applicable? | Rationale |
|----------|-------------|-----------|
| V7: Error handling, logging | Yes | Directly governs what gets logged, at what level, what must be excluded |
| V8: Data protection | Yes | Sensitive data (tokens, memory content, credentials) must not appear in traces |
| V2: Authentication | Yes | Auth flow instrumentation must not leak token values |
| V1, V3–V6, V9–V14 | No | Not applicable to an observability feature — no sessions, access control, crypto, file handling, or API changes in scope |

## ISO 27001:2022 Annex A Controls Reviewed

| Control | Applicable? | Rationale |
|---------|-------------|-----------|
| A.8.15: Logging | Yes | Security-relevant events must be captured at appropriate levels even under conservative settings |
| A.8.11: Data masking | Yes | Sensitive data redaction in trace output — maps to R-16/R-17/R-18 |
| A.8.16: Monitoring | Yes | OTLP export enables downstream anomaly detection and alerting |
| A.8.17: Clock synchronization | Handled | `tracing` crate uses system clock / `Instant` — no action needed |
| A.5.33: Protection of records | Deferred | Log integrity and tamper evidence are the collector/backend's responsibility; Phase 5 concern |
| A.8.10: Information deletion | Not applicable | Trace data retention is managed by the collector, not the application |
| A.8.12: Data leakage prevention | Covered | Overlaps with V8 / R-16–R-18 |

## Requirements

### Span conventions

| ID | Requirement | Source UC | Security Ref | Priority |
|----|-------------|-----------|--------------|----------|
| R-01 | Every public method in embedding, index, repo, and auth modules shall have a tracing span | UC-01, UC-06 | V7.1, A.8.15 | Must |
| R-02 | Span names shall use `snake_case` and follow the pattern `module.operation` (e.g. `embedding.embed`, `repo.push`, `index.search`) | UC-06, UC-07 | — | Must |
| R-03 | All span fields shall use structured values (no `format!` interpolation) | UC-01, UC-07 | V7.1 | Must |
| R-04 | Field names shall use `snake_case` and be consistent across modules (e.g. always `count`, not `num_results` in one place and `result_count` in another) | UC-06 | — | Must |
| R-05 | Subsystem spans shall nest under the parent MCP handler span, creating a clear hierarchy (handler → embed → index → repo) | UC-01, UC-07 | — | Must |

### Subsystem coverage

| ID | Requirement | Source UC | Security Ref | Priority |
|----|-------------|-----------|--------------|----------|
| R-06 | Embedding spans shall include: batch size, dimensions, per-chunk timing, model ID | UC-01 | — | Must |
| R-07 | Vector index spans shall include: operation, key count, result count, dimensions, scope | UC-01 | — | Must |
| R-08 | Git repo spans shall include: operation, branch name, file count; OIDs for push/pull | UC-01 | — | Must |
| R-09 | Auth spans shall include: resolution source tried (at `debug`), resolution source succeeded (at `info`), token provenance — never the token value | UC-01, SC-02 | V2.10, V7.1, A.8.15 | Must |
| R-10 | MCP handler spans shall include: session ID, content size, result count, scope, memory name | UC-01, UC-03 | V7.1, A.8.15 | Must |
| R-11 | Startup shall have timed spans for each subsystem initialization (repo open, embedding model load, index load) | UC-02 | — | Must |

### OTLP export

| ID | Requirement | Source UC | Security Ref | Priority |
|----|-------------|-----------|--------------|----------|
| R-12 | OTLP exporter shall be available behind `--features otlp` feature flag | UC-04 | — | Must |
| R-13 | When the `otlp` feature is enabled, the subscriber shall compose the `fmt` layer with an OpenTelemetry layer | UC-04 | A.8.16 | Must |
| R-14 | OTLP endpoint shall be configurable via environment variable (`OTEL_EXPORTER_OTLP_ENDPOINT`) | UC-04 | — | Must |
| R-15 | The default build (no `otlp` feature) shall not pull in OpenTelemetry dependencies | UC-04 | — | Must |

### Sensitive data protection

| ID | Requirement | Source UC | Security Ref | Priority |
|----|-------------|-----------|--------------|----------|
| R-16 | Auth tokens, git credentials, and API keys shall never appear in span fields at any log level | AC-01, SC-01 | V2.10, V8.3, A.8.11 | Must |
| R-17 | Memory content in spans shall be limited to a size field (byte count), never the full text | AC-01, SC-01 | V8.3, A.8.11 | Must |
| R-18 | Git remote URLs in spans shall use the existing `redact_url` function to strip credentials | AC-01, SC-01 | V8.3, A.8.11 | Must |

### Operability

| ID | Requirement | Source UC | Security Ref | Priority |
|----|-------------|-----------|--------------|----------|
| R-19 | Trace verbosity shall be controllable via `RUST_LOG` environment variable with per-module granularity | UC-05 | V7.1 | Must |
| R-20 | Default log level shall produce useful output without being noisy (info-level for handler spans, debug-level for subsystem internals) | UC-05, AC-02 | V7.1 | Should |
| R-21 | Security-relevant events (auth failures, invalid inputs, permission denials) shall be logged at `warn` or higher, not gated behind `debug` | SC-01, SC-02 | V7.2, A.8.15 | Must |

## Security Requirements Traceability Matrix (SRTM)

| Req ID | Requirement | Source UC | Security Ref | Test Case |
|--------|-------------|-----------|--------------|-----------|
| R-01 | Public methods have spans | UC-01, UC-06 | V7.1, A.8.15 | TC-01: verify span exists for each public method (grep/audit) |
| R-02 | Span naming convention `module.operation` | UC-06, UC-07 | — | TC-02: verify all span names match pattern |
| R-03 | Structured fields only | UC-01, UC-07 | V7.1 | TC-03: verify no `format!` in span field values (lint/audit) |
| R-04 | Consistent field naming | UC-06 | — | TC-04: verify field names consistent across modules (audit) |
| R-05 | Span hierarchy | UC-01, UC-07 | — | TC-05: `remember` call produces nested spans (handler → embed → index → repo) |
| R-06 | Embedding span fields | UC-01 | — | TC-06: embed span includes batch_size, dimensions, model |
| R-07 | Index span fields | UC-01 | — | TC-07: index.search span includes key_count, result_count |
| R-08 | Repo span fields | UC-01 | — | TC-08: repo.save span includes branch, file_count |
| R-09 | Auth span fields | UC-01, SC-02 | V2.10, V7.1, A.8.15 | TC-09: auth spans log source, never token value |
| R-10 | Handler span fields | UC-01, UC-03 | V7.1, A.8.15 | TC-10: handler spans include session_id, content_size |
| R-11 | Startup timing spans | UC-02 | — | TC-11: startup logs show timed init for each subsystem |
| R-12 | OTLP behind feature flag | UC-04 | — | TC-12: `cargo check` without `otlp` succeeds, no otel deps |
| R-13 | Composed subscriber | UC-04 | A.8.16 | TC-13: with `otlp`, subscriber has fmt + otel layers |
| R-14 | OTLP endpoint configurable | UC-04 | — | TC-14: `OTEL_EXPORTER_OTLP_ENDPOINT` respected |
| R-15 | No otel deps by default | UC-04 | — | TC-15: `cargo tree` without `otlp` shows no opentelemetry |
| R-16 | No tokens in spans | AC-01, SC-01 | V2.10, V8.3, A.8.11 | TC-16: auth resolution — captured spans contain no token values |
| R-17 | Content size only | AC-01, SC-01 | V8.3, A.8.11 | TC-17: remember — span has content_size, no content text |
| R-18 | Redacted git URLs | AC-01, SC-01 | V8.3, A.8.11 | TC-18: push/pull — span URL fields are redacted |
| R-19 | RUST_LOG per-module control | UC-05 | V7.1 | TC-19: per-module filtering works (existing EnvFilter) |
| R-20 | Appropriate default levels | UC-05, AC-02 | V7.1 | TC-20: default config — info handler spans, no debug noise |
| R-21 | Security events at warn+ | SC-01, SC-02 | V7.2, A.8.15 | TC-21: auth failure produces warn-level event |
