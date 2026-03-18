# Deployment Target

## Primary: Kubernetes
- Running in the `butterfly` namespace
- Behind Cilium Gateway API (HTTPRoute)
- HTTPS with fixed domain name
- TLS certificate managed by StepClusterIssuer (step-ca)
- TLS terminates at the gateway — binary serves plain HTTP
- AI agent k8s identity: `github:butterflysky-ai` with RBAC for secrets, services, pods, deployments, httproutes

## Token storage in k8s
- GitHub token stored as a Kubernetes Secret
- Mounted as env var `MEMORY_MCP_GITHUB_TOKEN` in the pod spec
- Existing auth chain (env var → file → keyring) handles this natively

## Client access
- Claude Code sessions connect via MCP server URL in `~/.claude.json`
- No local binary or local git repo needed on client devices
- Memories centralized in the k8s-hosted instance

## Auth subcommand plan
- `memory-mcp auth login` — OAuth device flow + auto-detect storage
- `memory-mcp auth login --store k8s-secret` — creates K8s Secret directly (kube crate, behind `k8s` cargo feature)
- `memory-mcp auth status` — reports which auth source resolved
