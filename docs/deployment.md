# Deployment

## Quick start (any Kubernetes cluster)

The manifests in `deploy/k8s/` are generic and work on any cluster.

### 1. Build and push the image

```bash
docker build -t YOUR_REGISTRY/memory-mcp:latest .
docker push YOUR_REGISTRY/memory-mcp:latest
```

Or let the [GitHub Actions workflow](../.github/workflows/build.yml) do it on
push to `main`. The workflow pushes to `ghcr.io/butterflyskies/memory-mcp`
using the built-in `GITHUB_TOKEN` — no extra secrets needed.

### 2. Apply the manifests

```bash
# Create namespace, RBAC, PVC, and Service
kubectl apply -f deploy/k8s/namespace.yml
kubectl apply -f deploy/k8s/rbac.yml
kubectl apply -f deploy/k8s/pvc.yml
kubectl apply -f deploy/k8s/service.yml
```

### 3. Create the GitHub token Secret

Option A — use the built-in auth subcommand. Run from a host with kubeconfig
access, or as a one-off Kubernetes Job using the `memory-mcp-bootstrap`
ServiceAccount (see `deploy/k8s/rbac.yml`):

```bash
memory-mcp auth login \
  --store k8s-secret \
  --k8s-namespace memory-mcp \
  --k8s-secret-name memory-mcp-github-token
```

Option B — apply the template manually after filling in the token:

```bash
# Replace <TOKEN> with your GitHub personal access token (repo scope)
kubectl create secret generic memory-mcp-github-token \
  --namespace memory-mcp \
  --from-literal=token=<TOKEN>
```

### 4. Deploy

Edit `deploy/k8s/deployment.yml`:
- Set `image:` to your registry/image:tag
- Uncomment and set `MEMORY_MCP_REMOTE_URL` to your private GitHub repository URL
- Uncomment the `MEMORY_MCP_GITHUB_TOKEN` secretKeyRef block (required when using a remote)

Then apply:

```bash
kubectl apply -f deploy/k8s/deployment.yml
```

### 5. Expose externally

The Service is ClusterIP only. Add an Ingress or Gateway API HTTPRoute to
expose the server outside the cluster. TLS should terminate at the gateway;
the binary serves plain HTTP on port 8080.

Example with a basic Ingress:

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: memory-mcp
  namespace: memory-mcp
spec:
  rules:
    - host: memories.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: memory-mcp
                port:
                  number: 8080
```

### 6. Connect Claude Code

Add to `~/.claude.json` (or your project-local `.mcp.json`):

```json
{
  "mcpServers": {
    "memory": {
      "type": "http",
      "url": "https://memories.example.com/mcp"
    }
  }
}
```

---

## Goddess Cluster (project-internal)

The goddess cluster uses a richer stack. The production manifests live in the
ArgoCD gitops repo, not here. Key differences from the generic path:

- **Registry**: `ghcr.io/butterflyskies/memory-mcp` (public); mirrored to
  `harbor.svc.echoes` if needed for air-gapped pulls
- **Namespace**: `memory-mcp`
- **Ingress**: Cilium Gateway API (HTTPRoute, not Ingress)
- **TLS**: `StepClusterIssuer` (step-ca) — certificate auto-provisioned
- **TLS termination**: at the gateway; pod serves plain HTTP
- **Domain**: `memories.svc.echoes`
- **GitOps**: ArgoCD Application syncing from the gitops repo
- **Auth identity**: `github:butterflysky-ai` via Dex OIDC; RBAC grants
  secrets/services/pods/deployments/httproutes in `memory-mcp`

To deploy a new version, push to `main` (or tag `v*`). The GitHub Actions
workflow builds and pushes to Harbor. ArgoCD picks up the new image tag
automatically (image updater) or on manual sync.
