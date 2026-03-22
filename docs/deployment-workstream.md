# Deployment Workstream: Kubernetes Production Readiness

Status: planning (not yet started)

## Goal

Deploy memory-mcp to the goddess Kubernetes cluster as the production memory
backend for all AI agent sessions. Replace Serena's memory layer while keeping
Serena's code intelligence tools.

## Prerequisites (done)

- [x] Container image builds in CI (`ghcr.io/butterflyskies/memory-mcp`)
- [x] Multi-stage Dockerfile with pre-warmed embedding model
- [x] Container hardening (non-root, read-only root fs, dropped caps, seccomp)
- [x] SLSA provenance + SBOM attestations
- [x] Generic k8s manifests in `deploy/k8s/`
- [x] `--features k8s` for `--store k8s-secret` token storage backend
- [x] Streamable HTTP transport (no stdio dependency)

## Phase 1: Manifest separation (generic vs cluster-specific)

The current manifests in `deploy/k8s/` mix generic and cluster-specific concerns.
Separate them using kustomize so the generic base works on any cluster.

### Base (`deploy/k8s/base/`)

These should work on any Kubernetes cluster with no modifications:

- `namespace.yml` — creates the namespace
- `rbac.yml` — ServiceAccounts (runtime + bootstrap), Roles, RoleBindings
- `pvc.yml` — PersistentVolumeClaim for the git-backed memory repo (no
  StorageClass specified — uses cluster default)
- `service.yml` — ClusterIP Service on port 8080
- `deployment.yml` — Deployment with security context, resource limits,
  health probes, volume mounts
- `kustomization.yaml` — ties them together

### Overlay (`deploy/k8s/overlays/goddess/`)

Cluster-specific configuration:

- `kustomization.yaml` — patches the base with goddess-specific values
- `httproute.yml` — Gateway API HTTPRoute for `gw-ext` gateway
- PVC patch: set `storageClassName: ceph-block` (matches cluster default,
  but explicit is better than implicit)
- Deployment patch: set image to `ghcr.io/butterflyskies/memory-mcp`,
  configure `MEMORY_MCP_REMOTE_URL`, mount the GitHub token secret
- Namespace patch: if deploying to `butterfly` instead of `memory-mcp`

## Phase 2: Gateway API + TLS

The goddess cluster uses Cilium Gateway API with StepClusterIssuer (step-ca)
for automatic TLS certificate provisioning.

### HTTPRoute

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: memory-mcp
spec:
  parentRefs:
    - name: gw-ext
      sectionName: https
  hostnames:
    - memories.svc.echoes  # or whatever domain is chosen
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /
      backendRefs:
        - name: memory-mcp
          port: 8080
```

### TLS considerations

- Certificate is managed by the gateway listener configuration, not by the
  application. The `gw-ext` gateway's HTTPS listener references a
  StepClusterIssuer — certs are provisioned automatically when the
  HTTPRoute attaches to the listener.
- The memory-mcp pod serves plain HTTP. TLS terminates at the gateway.
- No cert-manager Certificate resource needed in the application namespace
  (the gateway handles it).

## Phase 3: ArgoCD Application

Create an ArgoCD Application that syncs the goddess overlay:

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: memory-mcp
  namespace: argocd
spec:
  project: default
  source:
    repoURL: https://github.com/butterflyskies/memory-mcp.git
    targetRevision: main
    path: deploy/k8s/overlays/goddess
  destination:
    server: https://kubernetes.default.svc
    namespace: butterfly  # or memory-mcp, depending on namespace decision
  syncPolicy:
    automated:
      prune: true
      selfHeal: true
```

### Decisions needed

- **Namespace**: `butterfly` (shared with other workloads) or `memory-mcp`
  (dedicated)? The existing manifests use `memory-mcp`. Using `butterfly`
  would share the RBAC boundary with other AI agent infrastructure.
- **Image updates**: ArgoCD Image Updater for automatic rollouts on new
  image tags, or manual sync?
- **Should the ArgoCD Application live in this repo** (alongside the
  manifests) or in a separate gitops repo?

## Phase 4: Token bootstrap

The pod needs a GitHub token for git sync. Options:

1. **Pre-create the secret manually** — `kubectl create secret` with a PAT
2. **Use `memory-mcp auth login --store k8s-secret`** — run as a one-off
   Job using the bootstrap ServiceAccount
3. **Mount an existing secret** — if the token is already in the cluster
   from another workload

The existing RBAC in `deploy/k8s/rbac.yml` has a `memory-mcp-bootstrap`
ServiceAccount with permission to create/update secrets. Option 2 is the
intended flow.

## Phase 5: Migration (Serena → memory-mcp)

Import existing Serena memories into the memory-mcp store. Two sources:

### Global memories (`~/.serena/memories/`)

~20 memories covering environment variables, code standards, workflow
preferences, project standards, etc. These become `scope: global` memories
in memory-mcp.

### Project memories (`.serena/memories/` in each repo)

Project-specific context: session handoffs, overviews, deployment targets,
operational concerns. These become `scope: project:{repo-name}` memories.

### Migration approach

1. Write a script that reads each Serena memory file, extracts the name and
   content, and calls the `remember` MCP tool (or directly writes markdown
   files to the memory-mcp git repo in the expected format)
2. Verify with `list` and `recall` that all memories are searchable
3. Run a parallel session using both Serena memories and memory-mcp to
   validate parity

## Phase 6: Claude Code integration

### Update CLAUDE.md

Replace Serena memory references in `~/.claude/CLAUDE.md` with memory-mcp
tool calls:

| Serena | memory-mcp |
|--------|-----------|
| `read_memory` | `read` tool |
| `write_memory` | `remember` tool |
| `edit_memory` | `edit` tool |
| `delete_memory` | `forget` tool |
| `list_memories` | `list` tool |
| `rename_memory` | `forget` + `remember` (no native rename yet) |

### MCP server config

Add to `~/.claude.json`:

```json
{
  "mcpServers": {
    "memory": {
      "type": "http",
      "url": "https://memories.svc.echoes/mcp"
    }
  }
}
```

### Parallel run period

Run both Serena memories and memory-mcp for 2-3 sessions to validate:
- Session startup instructions work
- Memory reads/writes are reliable
- Semantic search (recall) finds relevant memories
- Latency is acceptable (network hop to k8s vs local file reads)
- Error handling when the server is temporarily unreachable

## Phase 7: Operational concerns

### Model weights mmap safety

The embedding model is memory-mapped from the HuggingFace cache. If the
cache is mutated while the server is running (e.g., by `huggingface-cli`),
the mmap'd region becomes undefined. The container image pre-warms the
model so this is only a concern if the cache volume is shared.

**Mitigation**: The container's HF_HOME is on its own volume, not shared.
Document in the operational runbook.

### Health probes

The deployment should include liveness and readiness probes. The MCP
endpoint itself can serve as a readiness check (POST to `/mcp` with
an MCP initialize request). A simpler option is a dedicated `/healthz`
endpoint — this doesn't exist yet and would need to be added.

### Resource sizing

- **Memory**: ~300MB baseline (model weights + index). Grows with memory
  count — plan for 512MB request, 1Gi limit as a starting point.
- **CPU**: Embedding computation is CPU-bound. Single-query latency is
  ~50ms on modern hardware. 250m request, 1 core limit.
- **Disk**: The git repo + HF model cache. 1Gi PVC should be generous
  for the foreseeable future.

### Backup

The git repo IS the backup — it syncs to a GitHub remote. If the PVC is
lost, re-clone from the remote. The vector index is rebuilt from the repo
on startup.
