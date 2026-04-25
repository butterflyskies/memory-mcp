# Design: Phase 2 Quality — Config, Testability, Index Abstraction & Health

Issues: #94, #145, #146, #164

## Summary

Designs the remaining Phase 2 work as a cohesive unit: vector index trait
abstraction, OAuth device flow provider abstraction, auth integration tests,
and `/readyz` health endpoint.

## Artifacts

| Artifact | Phase | Status |
|----------|-------|--------|
| [Problem Space](problem.md) | 1 | Approved |
| [Requirements & SRTM](requirements.md) | 2 | Approved |
| [Architecture](architecture.md) | 3 | Approved |
| [Threat Model](threat-model.md) | 4 (lightweight) | Approved |
| [Test Plan](test-plan.md) | 5 | Approved |

## Related

- [ADR-0024: `auth::oauth::DeviceFlowProvider`](../../adr/0024-oauth-device-flow-provider-trait.md)
- [ADR-0012: OAuth device flow for token acquisition](../../adr/0012-oauth-device-flow-token-acquisition.md)
- [Tracing Scaffold Design](../tracing/) (prior Phase 2 work)

## Key Decisions

- **VectorStore trait** at the semantic level (`Box<dyn VectorStore>` in AppState),
  with usearch as the first concrete implementation
- **`auth::oauth::DeviceFlowProvider`** trait scoped to RFC 8628 device flow —
  module namespace provides OAuth context (ADR-0024)
- **Two testing levels** for the index: private failure injection (rollback mechanics)
  and public trait-level error behavior
- **Readiness checks** validate consistency (embedding ↔ index dimensions), not
  magic numbers — forward-compatible with model upgrades
- **Remote sync** does not affect readiness by default (opt-in via `--require-remote-sync`)
