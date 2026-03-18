# Session Handoff — memory-mcp

## Canonical tracking
- **[TODO.md](https://github.com/butterflyskies/memory-mcp/blob/main/TODO.md)** — single source of truth for task status
- **butterflyskies/tasks#80** — discussion thread, references TODO.md (keep reopening if auto-closed on PR merge)

## Current state
On `main`, Phase 2 roughly half complete (auth + sync done, migration + deployment remaining).

## Ordering intent
Deploy to k8s first, then wire up as MCP server in Claude Code. The server needs to be running and reachable before pointing clients at it.

## Context worth preserving
- Review cycles take 2-3 passes — pre-flight checklist helped but P3s still emerge
- IDE diagnostics are frequently stale — always verify with `cargo check`
- Never amend commits — always add new commits on top
- Never silently fall back to plaintext file storage for credentials
- `capture_head_oid` used in fast_forward but NOT in merge_with_remote (defensive)
