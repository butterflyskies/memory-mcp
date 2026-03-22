# Deployment Target

## Primary: Kubernetes (goddess cluster)

### What's done
- Container image: `ghcr.io/butterflyskies/memory-mcp` built in CI, multi-stage with pre-warmed model
- K8s manifests in `deploy/k8s/`: namespace, rbac, pvc, service, deployment, secret
- Container hardened: non-root, readOnlyRootFilesystem, drop ALL caps, seccomp RuntimeDefault
- SLSA provenance + SBOM attestations on every image push
- `--features k8s` for `--store k8s-secret` token storage backend
- AI agent k8s identity: `github:butterflysky-ai` with RBAC for secrets, services, pods, deployments, httproutes
- Namespace: `butterfly`

### What's NOT done
- **ArgoCD Application manifest** — no GitOps deployment yet, manifests exist but aren't wired to ArgoCD
- **Gateway API HTTPRoute** — manifests use generic Service but no HTTPRoute defined for external access
- **TLS** — StepClusterIssuer manages certs via gateway `gw-ext` listener config; not configured for memory-mcp yet
- **Storage class** — PVC exists but may not specify `ceph-block` (the cluster default StorageClass)
- **Abstraction** — current manifests mix generic concerns with goddess-cluster-specific concerns; need to separate:
  - Generic: deployment spec, service, rbac, container config
  - Cluster-specific: StorageClass (`ceph-block`), Gateway API config, cert management, ArgoCD app

### Cluster environment specifics (goddess)
- **Ingress**: Gateway API only, never Ingress. Gateway name: `gw-ext`
- **Certs**: StepClusterIssuer (step-ca), typically managed within the gateway definition derived from listener configs and HTTPRoute
- **Storage**: `ceph-block` is the default StorageClass
- **GitOps**: ArgoCD for all deployments
- **TLS termination**: at the gateway — the binary serves plain HTTP internally

### Token storage in k8s
- GitHub token stored as a Kubernetes Secret
- Mounted as env var `MEMORY_MCP_GITHUB_TOKEN` in the pod spec
- Existing auth chain (env var → file → keyring) handles this natively

### Client access
- Claude Code sessions connect via MCP server URL in `~/.claude.json`
- No local binary or local git repo needed on client devices
- Memories centralized in the k8s-hosted instance

### Deployment work items needed
1. Review and update PVC to confirm ceph-block StorageClass
2. Create HTTPRoute for memory-mcp behind gw-ext gateway
3. Verify TLS cert provisioning via StepClusterIssuer
4. Create ArgoCD Application manifest
5. Separate generic manifests from goddess-cluster overlays (kustomize or similar)
6. Test end-to-end: deploy → auth → remember → recall → sync
7. Operational runbook: model weights mmap concern (see operational_concerns memory)
