# ADR-0035: Recall feedback loop via mark_applied and recall-stats

## Status
Accepted

## Context
ADR-0034 introduced the recall event log with columns for feedback data, but no mechanism
for agents to report whether recalled memories were actually useful. Without a closed loop,
the telemetry data cannot drive threshold calibration.

## Decision
Three feedback mechanisms, layered:

1. **`mark_applied` MCP tool** — agents explicitly report which recalled memories influenced
   their session, with a confidence level (`applied`, `maybe`, `not_applied`) and optional
   note. Keyed by `recall_id` from the recall response.

2. **Read-recall correlation** — when `read` is called for a memory that was recently recalled
   in the same session, the handler auto-marks `was_read=1` on the recall event. Fire-and-forget,
   non-blocking. This provides passive signal without requiring agent cooperation.

3. **`recall-stats` CLI** — `memory-mcp recall-stats` prints precision-by-distance buckets,
   showing what fraction of recalls at each distance range were actually applied. Enables
   empirical threshold tuning.

Server instructions explicitly guide agents to call `mark_applied` after acting on recalls.

## Consequences
- Agents that ignore `mark_applied` still contribute passive signal via read correlation
- Distance threshold recommendations become data-driven rather than guesswork
- `recall_stats` output is human-readable — no dashboard infrastructure needed
- The feedback data stays local (SQLite) — no privacy concerns from syncing usage patterns

## References
- #240, #246
