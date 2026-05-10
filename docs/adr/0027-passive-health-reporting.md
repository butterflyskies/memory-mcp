# ADR-0027: Passive health reporting via subsystem reporters

## Status
Accepted

## Context
The `/readyz` endpoint (issue #164) needs to answer "can this instance serve requests?"
for Kubernetes readiness probes. An active-probing approach (handler queries each
subsystem on every request) introduces contention: acquiring the repo mutex, calling
trait methods, potentially blocking the async runtime with filesystem I/O. It also
inverts responsibility — the probe must understand each subsystem's health semantics.

## Decision
Each subsystem (git repo, embedding engine, vector index) holds a `SubsystemReporter`
that writes to a shared `ArcSwap<SubsystemStatus>` during normal operations. The
`/readyz` handler reads the three snapshots — zero probing, zero contention, zero I/O.

Health is reported at operation boundaries in the concrete implementations:
- `MemoryRepo`: after local git operations (read, write, delete, list). Remote sync
  failures do NOT affect readiness (per requirement R-21).
- `CandleEmbeddingEngine`: after each embed response from the worker channel.
- `UsearchStore`: after add/search operations.

Subsystems start in "not yet checked" (unhealthy) state. Successful construction
during startup reports initial health before the HTTP listener opens.

Transition-based logging uses `compare_exchange` on a shared `AtomicBool` to emit
`warn` only on ready-to-not-ready transitions, avoiding probe-interval log spam.

## Consequences
- `/readyz` is wait-free: three atomic loads + JSON serialization.
- No mutex contention between the probe and real operations.
- Health reflects actual operational reality, not synthetic checks.
- Adding a new subsystem requires: add a reporter to `HealthRegistry`, pass it to
  the implementation, call `report_ok`/`report_err` at operation boundaries.
- The `arc-swap` crate is added as a runtime dependency (~2K lines, well-maintained).
- Extends ADR-0006 (structured observability) to health reporting.
- Complements ADR-0026 (channel worker) — the embed worker's response path is a
  natural reporting site.
