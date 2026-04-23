<!-- design-meta
status: approved
last-updated: 2026-04-23
-->

# Design: Tracing Scaffold (#52)

Comprehensive structured tracing across all memory-mcp subsystems, with
optional OTLP export behind a feature flag. This is the load-bearing
observability foundation for Phase 2 — every subsequent feature gets
spans for free by following the conventions established here.

## Artifacts

| Document | Phase | Status | Description |
|----------|-------|--------|-------------|
| [Problem Space](problem.md) | 1 | Approved | What we're solving, current gaps, success criteria |
| [Requirements](requirements.md) | 2 | Approved | Use cases, requirements (R-01–R-21), ASVS/ISO review, SRTM |
| [Architecture](architecture.md) | 3 | Approved | 9 decisions, field dictionary, Mermaid diagrams, audit vs. privacy tension |
| [Threat Model](threat-model.md) | 4 | Approved | Lightweight review — trace output boundary. Full STRIDE deferred to #115 |
| [Test Plan](test-plan.md) | 5 | Approved | 21 test cases (TC-01–TC-21), CI integration |

## Key decisions

- Span naming: `module.operation` (e.g. `handler.remember`, `embedding.embed`)
- Field naming: flat `snake_case`, canonical dictionary in architecture.md
- Sensitive data: redact at source, never in spans (tokens, content, credentials)
- OTLP: behind `--features otlp`, zero dep cost for default builds
- Audit content (query text, memory content): excluded from scaffold, deferred to Phase 5 tiered output — see architecture.md "Design Tension" section

## Forward references

- **#162** — W3C Trace Context propagation (Phase 5)
- **#115** — Full STRIDE threat model required (Phase 6)
- **#110, #111, #112** — Audit logging fields, informed by audit vs. privacy tension
