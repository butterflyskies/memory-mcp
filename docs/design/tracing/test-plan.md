<!-- design-meta
status: draft
last-updated: 2026-04-23
phase: 5
-->

# Test Plan: Tracing Scaffold (#52)

*Draft — 2026-04-23*

## Test infrastructure

Span-capturing tests use a custom in-memory subscriber layer that
collects spans and events for assertion. This infrastructure is built
as part of this work and can be extended by #146 (integration tests).

## Test cases

### Span conventions

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-01 | R-01 | Integration | Exercise every public method in embedding, index, repo, auth — assert the expected span name was emitted | In-memory subscriber |
| TC-02 | R-02 | Integration | Assert all captured span names match `^[a-z_]+\.[a-z_]+$` | In-memory subscriber |
| TC-03 | R-03 | Lint | Scan span macro invocations for `format!` in field values | CI grep/script |
| TC-04 | R-04 | Integration | Capture all field names across all spans, assert every name exists in a canonical allowlist derived from the field dictionary | In-memory subscriber |

### Span hierarchy

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-05 | R-05 | Integration | `remember` call produces nested spans: `handler.remember` → `embedding.embed` → `index.add` → `repo.save` — assert parent-child relationships | In-memory subscriber |

### Subsystem span fields

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-06 | R-06 | Unit | `embedding.embed` span includes `batch_size`, `dimensions`, `model` fields | In-memory subscriber |
| TC-07 | R-07 | Unit | `index.search` span includes `count`, `key_count`, `dimensions` fields | In-memory subscriber |
| TC-08 | R-08 | Unit | `repo.save` span includes `branch` field | In-memory subscriber |
| TC-09 | R-09 | Unit | `auth.resolve` span includes `token_source` field, no field contains a token value | In-memory subscriber |
| TC-10 | R-10 | Integration | Handler span includes `session_id` from `mcp-session-id` header, plus `name`, `scope`, `content_size` as appropriate | In-memory subscriber |
| TC-11 | R-11 | Integration | Startup emits timed spans for `repo.init`, `embedding.load_model`, `index.load` | In-memory subscriber |

### OTLP feature flag

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-12 | R-12 | Build | `cargo check` without `otlp` feature succeeds | CI workflow |
| TC-13 | R-13 | Build | `cargo check --features otlp` succeeds | CI workflow |
| TC-14 | R-14 | Integration | With `otlp` feature and a local collector, verify spans arrive at configured `OTEL_EXPORTER_OTLP_ENDPOINT` | CI workflow with collector sidecar |
| TC-15 | R-15 | Build | `cargo tree` without `otlp` feature shows no `opentelemetry` crates | CI workflow |

### Sensitive data protection

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-16 | R-16 | Unit | Set a known token value in env, trigger `auth.resolve`, capture all spans — assert no span field contains the token string | In-memory subscriber |
| TC-17 | R-17 | Unit | Call `remember` with known content, capture spans — assert `content_size` field present, no field contains the content string | In-memory subscriber |
| TC-18 | R-18 | Unit | Trigger repo operation with a URL containing credentials (`https://user:pass@host`), capture spans — assert URL fields are redacted | In-memory subscriber |

### Operability

| Test ID | Requirement | Type | Description | Mechanism |
|---------|-------------|------|-------------|-----------|
| TC-19 | R-19 | Unit | Verify `EnvFilter` respects per-module `RUST_LOG` settings (existing behavior, regression test) | In-memory subscriber |
| TC-20 | R-20 | Unit | With default filter (`memory_mcp=info`), handler spans are captured, subsystem debug spans are not | In-memory subscriber |
| TC-21 | R-21 | Unit | Trigger an auth failure, capture events — assert at least one event at `WARN` level | In-memory subscriber |

## CI integration

Add to the existing `build.yml` workflow:

- `cargo check --features otlp` — verifies OTLP build compiles (TC-12, TC-13)
- `cargo tree` assertion — verifies no opentelemetry crates without the feature (TC-15)
- Span convention lint script for TC-03
- All unit/integration tests above run as part of `cargo nextest` (existing CI step)
