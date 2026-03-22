# Session Handoff — memory-mcp

## What just shipped
- PR #64: fat lib / thin binary refactor closing 7 issues (#61, #65, #66, #67, #68, #70, #72)
- PR #73: cargo-semver-checks as required CI check (8 required checks total)
- PR #74: README accuracy fixes (token path, cargo install, TODO overhaul)
- PR #75: MCP client config snippets for 7 editors
- v0.3.0 published to crates.io (first publish, manual — Trusted Publishing not yet configured)

## Open PRs
- PR #76: deployment workstream doc + Serena memory updates — needs CI + merge

## Next up: k8s deployment workstream
Detailed plan in `docs/deployment-workstream.md`. Seven phases:
1. Manifest separation (kustomize base + goddess overlay)
2. Gateway API + TLS (StepClusterIssuer, HTTPRoute for gw-ext)
3. ArgoCD Application
4. Token bootstrap
5. Serena → memory-mcp migration
6. Claude Code integration (CLAUDE.md rewrite)
7. Operational concerns (health probes, resource sizing)

Key decisions needed:
- Namespace: `butterfly` (shared) or `memory-mcp` (dedicated)?
- Image updates: ArgoCD Image Updater or manual sync?
- ArgoCD app: in this repo or separate gitops repo?

## Also pending
- #62: Trusted Publishing (OIDC from GitHub Actions) — first manual publish done, need to configure TP in crates.io web UI
- #69: atomic file writes / symlink safety in token I/O — pre-existing, surfaced in code review
- cc-toolgate: 12 issues filed (#24-#35) — no Rust CI is the critical gap

## Operational notes
- semver-checks requires a crates.io baseline — won't work until the crate is published. Future versions will compare against the last published release automatically.
- `cargo publish` ships source as-is; feature flags are resolved by consumers, not at publish time
- The `docs/deployment.md` goddess cluster section describes a future state that isn't built yet — `docs/deployment-workstream.md` is the plan to get there
