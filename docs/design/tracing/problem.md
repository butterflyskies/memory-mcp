<!-- design-meta
status: draft
last-updated: 2026-04-22
phase: 1
-->

# Problem Space: Tracing Scaffold (#52)

*Draft — 2026-04-22*

## What are we solving?

memory-mcp has minimal, inconsistent observability. The MCP tool handlers in
`server.rs` have basic spans with timing fields (`embed_ms`, `repo_ms`), but the
subsystems they delegate to — embedding, vector index, git repo, auth — are opaque.
When something goes wrong in production, debugging requires guessing which
subsystem failed and why, rather than following structured trace data.

An AI agent calling the MCP server can't inspect internal state. The operator's
only window is trace output — and right now that window is narrow and inconsistent.

## Why now?

Phase 2 is starting. The roadmap comment on #132 explicitly front-loads the tracing
scaffold because every subsequent feature (#94 vector index trait, #146 integration
tests, #109 secret scanning) benefits from having spans already in place. Getting
conventions right now prevents each feature from inventing its own instrumentation
style.

## Current state

| Module | File(s) | Current tracing | Gap |
|--------|---------|-----------------|-----|
| MCP handlers | `server.rs` | Manual `info_span!` per tool, timing fields for embed/repo | No content sizes, result counts, scope values in spans. No span for `incremental_reindex`. |
| Embedding | `embedding/candle.rs` | `info!` for model loading, `warn!` for mutex poison | No spans for `embed`/`embed_one`, no batch size, no per-chunk timing |
| Vector index | `index.rs` | `warn!` on errors only | No spans for `add`/`remove`/`search`/`save`/`load`. No key counts, result counts, dimensions |
| Git repo | `repo.rs` | `info!`/`warn!` for push/pull events | No spans for `save_memory`/`read_memory`/`list_memories`/`push`/`pull`/`init_or_open`. No branch, OID, or file count fields |
| Auth | `auth.rs` | None | No spans for token resolution path, device flow steps |
| Startup | `main.rs` | `info!` for bind address and repo path | No timing for subsystem init (repo open, model load, index load) |

## Inputs and outputs

- **Input:** The existing codebase with ad-hoc `info!`/`warn!` logging
- **Output:** Structured, hierarchical spans with machine-parseable fields across
  all subsystems, plus an opt-in OTLP exporter for forwarding to Jaeger, Grafana
  Tempo, Honeycomb, or other OpenTelemetry-compatible backends
- **Key transformation:** Replace scattered log events with wide spans that capture
  context as structured fields, creating a consistent instrumentation convention
  that new code follows naturally

## Boundaries

### In scope

- Structured spans (`#[instrument]` + manual spans) across all subsystems
- Consistent field naming conventions for the whole codebase
- MCP session ID (rmcp-internal) as a structured field on handler spans
- OTLP export behind a `--features otlp` feature flag
- Ensuring the subscriber setup in `main.rs` supports layered composition
  (fmt + optional OTLP)

### Out of scope (deferred with tracking)

- **W3C Trace Context propagation** — extracting `traceparent`/`tracestate` from
  incoming requests so memory-mcp spans nest under the caller's trace. Tracked as
  #162, scheduled for Phase 5. The scaffold should not preclude this.
- **Caller-supplied metadata** — agent session names, project context, accounting
  fields plumbed through as span fields. Phase 6 enterprise concern. The trace
  architecture should be extensible enough that adding propagation and metadata
  extraction later is straightforward (Axum middleware + OpenTelemetry extractors).

### Out of scope (separate concerns)

- Prometheus metrics endpoint — #165
- `/readyz` health endpoint with subsystem checks — #164
- Performance benchmarking of tracing overhead (PR-level concern for #52)

### Constraints

- Structured fields only — no `format!` interpolation in span fields
- Must not log sensitive data: auth tokens, memory content beyond snippets,
  git credentials (URLs are already redacted via `redact_url` in `repo.rs`)
- OTLP export must be feature-gated to avoid pulling OpenTelemetry deps for
  users who don't need them
- Tracing overhead must be negligible for the default `fmt` subscriber

## Success criteria

1. Every public method in embedding, index, repo, and auth has a span with
   relevant structured fields
2. An operator can trace a single MCP tool call from handler through
   embedding → index → repo, seeing timing and context at each level
3. `RUST_LOG=memory_mcp=debug` produces useful, structured output for any
   operation
4. OTLP export works behind `--features otlp` and can send spans to a local
   collector
5. New code added in subsequent Phase 2 issues (#94, #146) gets spans by
   following the established conventions
6. No sensitive data (tokens, full memory content, git credentials) appears
   in any span field
7. MCP session ID is present on every handler span
