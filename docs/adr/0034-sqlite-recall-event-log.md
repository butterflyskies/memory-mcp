# ADR-0034: SQLite recall event log for threshold calibration

## Status
Accepted

## Context
Recall returns results ranked by vector distance, but there is no empirical data on which
distance thresholds produce useful results. Agents guess at cutoffs (e.g. "ignore above 0.42")
with no feedback loop. Without ground truth on recall quality, the system cannot self-calibrate.

## Decision
Add a local SQLite database (via `rusqlite`, bundled) as an append-only event log for recall
operations. Each recall result is logged with session_id, rank, distance, scope, and memory
name. A `recall_id` (UUID prefixed with `r_`) is returned in every recall response for
correlation. Schema includes `was_read`, `was_applied`, and `confidence` columns for the
feedback loop (populated by `mark_applied` and read-recall correlation).

Design choices:
- **SQLite only** — single source of truth for telemetry; no frontmatter
- **`Mutex<Connection>`** — rusqlite Connection is Send but not Sync; Mutex is minimal
- **`Option<RecallLog>` in AppState** — tests pass `None`, prod initializes the real log
- **WAL mode + NORMAL sync** — good write throughput for append-only workload
- **Non-fatal logging** — failures are warned, never propagated to the caller

## Consequences
- New `rusqlite` dependency (bundled SQLite, no system library required)
- Recall responses grow by one field (`recall_id`) — clients that ignore it are unaffected
- Telemetry data is local-only and not synced via git (unlike memories)

## References
- #231
