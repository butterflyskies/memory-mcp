# Session Handoff — memory-mcp

## Canonical tracking
- **TODO.md** in repo root — Phase 2 checklist with completion status
- **butterflyskies/tasks#80** — tracking issue with PR links and status comments

## Current state
On `main`, all merged PRs up to date. Active work: keyring-based token storage (ADR-0010).

## What's in flight
- `/develop` workflow for keyring token storage — plan approved, ready for Phase 2 (implement)
- Plan: add `keyring` crate, insert as step 3 in auth resolution chain (env → file → keyring → error), add `--set-token` CLI flag, tests

## Context worth preserving
- Review cycles consistently take 2 passes (initial + re-review after fixes)
- Pre-flight checklist added to develop skill (claude-skills PR #9) — watch if it reduces review cycles
- IDE diagnostics are frequently stale — always verify with `cargo check`, not IDE
- `capture_head_oid` is used in fast_forward but intentionally NOT in merge_with_remote (defensive: merge path should error on unborn HEAD, not silently sentinel)