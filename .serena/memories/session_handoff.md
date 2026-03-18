# Session Handoff — memory-mcp

## Canonical tracking
- **TODO.md** in repo root — Phase 2 checklist with completion status
- **butterflyskies/tasks#80** — tracking issue with PR links and status comments (keep reopening — auto-closes on PR merge)

## Current state
On `main`, all Phase 2 work through auth subcommand merged (PRs #1–#10).

## What's next
- `--store k8s-secret` backend (cargo feature-gated)
- Extract GitHub OAuth client ID from hardcoded const into config file/module (follow-up on tasks#80)
- Integration tests for `auth login`, `auth status`, `MEMORY_MCP_BIND` env var (follow-up on tasks#80)
- Migration tools (Serena global, Serena project, Claude Code auto-memories)
- Configure as MCP server in `~/.claude.json`
- K8s deployment: Cilium Gateway API, StepClusterIssuer certs

## Context worth preserving
- Review cycles take 2-3 passes — pre-flight checklist helped but P3s still emerge
- IDE diagnostics are frequently stale — always verify with `cargo check`
- `capture_head_oid` used in fast_forward but NOT in merge_with_remote (defensive)
- User strongly prefers no tech debt, even P3-level findings get fixed
- Never amend commits — always add new commits on top
- Never silently fall back to plaintext file storage for credentials
