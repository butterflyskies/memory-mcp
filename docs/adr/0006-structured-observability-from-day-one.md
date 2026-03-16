# ADR-0006: Structured Observability From Day One

## Status
Accepted

## Context
Tool handlers are the primary interface for AI agents. When recall returns fewer
results than expected, or remember takes too long, the calling agent has no way to
diagnose why without instrumentation. Adding observability after the fact means
retrofitting every code path and discovering blind spots in production.

## Decision
Every tool handler emits structured `tracing` spans with timing and operational
metrics (result counts, filter stats, latency breakdowns). The `recall` tool includes
filter transparency in its response: `pre_filter_count`, `filtered_by_scope`, `count`,
and `limit` — so the calling agent can detect when scope filtering drops results and
self-adjust by increasing the limit.

## Consequences
- Slight code overhead per handler (span creation, Instant timing)
- All operations are diagnosable from stderr logs without code changes
- Calling agents get enough metadata to make informed retry/tuning decisions
- Future: tracing spans are compatible with OpenTelemetry for k8s dashboards
- The recall response contract includes metadata fields that callers may depend on
