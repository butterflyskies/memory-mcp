<!-- design-meta
status: draft
last-updated: 2026-04-23
phase: 5
-->

# Test Plan: Tracing Scaffold (#52)

*Draft — 2026-04-23*

## Test infrastructure

Span-capturing tests use `tracing-test` or a custom in-memory subscriber
layer that collects spans and events for assertion. This infrastructure is
built as part of this work and can be extended by #146 (integration tests).

## Test cases

### Span conventions (audit/lint)

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-01 | R-01 | Audit | Every public method in embedding, index, repo, auth has a span — verified by grep/review | Manual (PR review checklist) |
| TC-02 | R-02 | Audit | All span names match `module.operation` pattern | Manual (PR review checklist) |
| TC-03 | R-03 | Audit | No `format!` interpolation in span field values | Manual (PR review checklist) |
| TC-04 | R-04 | Audit | Field names consistent with dictionary in architecture.md | Manual (PR review checklist) |

### Span hierarchy

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-05 | R-05 | Integration | `remember` call produces nested spans: `handler.remember` → `embedding.embed` → `index.add` → `repo.save`. Capture spans with in-memory subscriber, assert parent-child relationships. | Yes |

### Subsystem span fields

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-06 | R-06 | Unit | `embedding.embed` span includes `batch_size`, `dimensions`, `model` fields | Yes |
| TC-07 | R-07 | Unit | `index.search` span includes `count`, `key_count`, `dimensions` fields | Yes |
| TC-08 | R-08 | Unit | `repo.save` span includes `branch` field | Yes |
| TC-09 | R-09 | Unit | `auth.resolve` span includes `token_source` field, does not contain any field matching a token pattern | Yes |
| TC-10 | R-10 | Integration | Handler span includes `session_id` extracted from `mcp-session-id` header, plus `name`, `scope`, `content_size` as appropriate | Yes |
| TC-11 | R-11 | Integration | Startup emits timed spans for `repo.init`, `embedding.load_model`, `index.load` | Yes |

### OTLP feature flag

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-12 | R-12 | Build | `cargo check` without `otlp` feature succeeds | Yes (CI) |
| TC-13 | R-13 | Build | `cargo check --features otlp` succeeds | Yes (CI) |
| TC-14 | R-14 | Manual | With `otlp` feature and a local collector, verify spans arrive at configured `OTEL_EXPORTER_OTLP_ENDPOINT` | Manual |
| TC-15 | R-15 | Build | `cargo tree` without `otlp` feature shows no `opentelemetry` crates in dependency tree | Yes (CI) |

### Sensitive data protection

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-16 | R-16 | Unit | Set a known token value in env, trigger `auth.resolve`, capture all spans — assert no span field contains the token string | Yes |
| TC-17 | R-17 | Unit | Call `remember` with known content, capture spans — assert `content_size` field present, no field contains the content string | Yes |
| TC-18 | R-18 | Unit | Trigger repo operation with a URL containing credentials (`https://user:pass@host`), capture spans — assert URL fields are redacted | Yes |

### Operability

| Test ID | Requirement | Type | Description | Automated? |
|---------|-------------|------|-------------|------------|
| TC-19 | R-19 | Unit | Verify `EnvFilter` respects per-module `RUST_LOG` settings (existing behavior, regression test) | Yes |
| TC-20 | R-20 | Unit | With default filter (`memory_mcp=info`), handler spans are captured, subsystem debug spans are not | Yes |
| TC-21 | R-21 | Unit | Trigger an auth failure, capture events — assert at least one event at `WARN` level | Yes |

## CI integration

Add to the existing `build.yml` workflow:

- `cargo check --features otlp` — verifies OTLP build compiles (TC-12, TC-13)
- `cargo tree` assertion — verifies no opentelemetry crates without the feature (TC-15)
- All unit/integration tests above run as part of `cargo nextest` (existing CI step)

## PR review checklist

TC-01 through TC-04 are manual audit checks. Include in the PR description
as a checklist:

```
- [ ] Every public method in embedding, index, repo, auth has a span (TC-01/R-01)
- [ ] All span names follow `module.operation` (TC-02/R-02)
- [ ] No `format!` in span field values (TC-03/R-03)
- [ ] Field names match architecture.md dictionary (TC-04/R-04)
```
