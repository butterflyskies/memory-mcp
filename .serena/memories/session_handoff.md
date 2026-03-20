# Session Handoff — memory-mcp

## What just shipped
- PR #43: cross-platform CI (Linux + macOS `cargo build`) + vendored OpenSSL + Windows removed from matrices
- PR #45: conditional OpenSSL vendoring (non-Linux only) + Node.js 24 opt-in for release-please
- v0.1.5 release cut with `Release-As: 0.1.5` — check that release pipeline completed (draft → binaries → undraft)

## Open PRs
- PR #47 (memory-mcp): project overview memory update — trivial, ready to merge
- PR #4 (flight-log): evening flight log entry
- PR #5 (rust-template): accumulated CI improvements — needs cross-platform CI ported from memory-mcp

## Pending items
- **rust-template**: port the cross-compile job and conditional OpenSSL vendoring from memory-mcp
- **v0.1.5 release verification**: confirm macOS binary was uploaded and release was undrafted
- **Node.js 24 compatibility**: first real test happens on next release-please run — watch for failures
- **cc-toolgate#19 / tasks#90**: config_env process environment fallback — on Friday Focus milestone

## Context
- Windows builds are blocked by usearch v2.24.0 MAP_FAILED incompatibility (#42) — no upstream fix available
- Git identity must use AI account for all pushes (user can't approve their own PRs)
- User prefers new commits over amend + force-push
