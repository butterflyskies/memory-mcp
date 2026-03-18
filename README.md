# memory-mcp

A semantic memory server for AI coding agents. Memories are stored as markdown files in a git repository and indexed for semantic retrieval using local embeddings — no API keys, no cloud dependency for inference.

Built on the [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) so any compatible agent (Claude Code, Cursor, Windsurf, custom agents) can remember, recall, and sync knowledge across sessions and devices.

## Why

AI coding agents are stateless between sessions. They lose context about your preferences, your codebase's architecture, past decisions, and hard-won debugging knowledge. memory-mcp gives agents a persistent, searchable memory that:

- **Survives across sessions** — what an agent learned yesterday is available today
- **Syncs across devices** — git push/pull keeps memories consistent everywhere
- **Stays private** — embeddings run locally (no data leaves your machine), storage is a git repo you control
- **Scales with you** — semantic search finds relevant memories even as the collection grows into hundreds or thousands

## Quick start

### From source

```bash
git clone https://github.com/butterflyskies/memory-mcp.git
cd memory-mcp
cargo build --release
```

### Run the server

```bash
# Starts on 127.0.0.1:8080 with a local git repo at ~/.memory-mcp
./target/release/memory-mcp serve

# Or configure via environment variables
MEMORY_MCP_BIND=0.0.0.0:9090 \
MEMORY_MCP_REPO_PATH=/path/to/memories \
MEMORY_MCP_EMBEDDING_MODEL=BGESmallENV15 \
./target/release/memory-mcp serve
```

### Connect Claude Code

Add to `~/.claude.json` or your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "memory": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

The agent can now use `remember`, `recall`, `read`, `edit`, `forget`, `list`, and `sync` as tools.

## Tools

| Tool | Description |
|------|-------------|
| **remember** | Store a new memory with content, name, tags, and scope. Embeds and indexes it for semantic search. |
| **recall** | Search memories by natural-language query. Returns the top matches ranked by semantic similarity. |
| **read** | Fetch a specific memory by name with full content and metadata. |
| **edit** | Update an existing memory. Supports partial updates — omit fields to preserve them. |
| **forget** | Delete a memory by name. Removes from git and the search index. |
| **list** | Browse all memories, optionally filtered by scope. |
| **sync** | Push/pull the memory repo with a git remote. Handles conflicts via recency-based resolution. |

### Example: agent remembers a debugging insight

```
Tool: remember
{
  "name": "postgres/connection-pool-timeout",
  "content": "When the connection pool times out under load, the issue is usually...",
  "tags": ["postgres", "debugging", "performance"],
  "scope": "project:my-api"
}
```

### Example: agent recalls relevant context

```
Tool: recall
{
  "query": "database connection issues under high load",
  "scope": "project:my-api",
  "limit": 5
}
```

## How it works

```
Agent ──MCP──▶ memory-mcp ──▶ fastembed (local embeddings)
                    │                    │
                    ▼                    ▼
              git repo            usearch HNSW index
            (markdown files)    (semantic search)
                    │
                    ▼
              git remote
            (sync across devices)
```

1. **Storage**: memories are markdown files with YAML frontmatter (tags, scope, timestamps) committed to a local git repository
2. **Embeddings**: content is embedded locally using [fastembed](https://github.com/Anush008/fastembed-rs) — no external API calls
3. **Search**: embeddings are indexed in an HNSW graph ([usearch](https://github.com/unum-cloud/usearch)) for fast approximate nearest-neighbor search
4. **Sync**: the git repo can push/pull to a remote (GitHub, GitLab, etc.) for cross-device sync with automatic conflict resolution
5. **Auth**: GitHub tokens via OAuth device flow (`memory-mcp auth login`), stored in the system keyring or a Kubernetes Secret

### Memory format

```markdown
---
id: 550e8400-e29b-41d4-a716-446655440000
name: postgres/connection-pool-timeout
tags: [postgres, debugging, performance]
scope:
  type: Project
  name: my-api
created_at: 2026-03-18T12:00:00Z
updated_at: 2026-03-18T12:00:00Z
source: debugging-session
---

When the connection pool times out under load, the issue is usually...
```

### Scoping

Memories are scoped to control visibility:

- **`global`** — available to all projects (preferences, standards, general knowledge)
- **`project:{name}`** — scoped to a specific project (architecture decisions, debugging context, team conventions)

## Configuration

All options can be set via CLI flags or environment variables:

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--bind` | `MEMORY_MCP_BIND` | `127.0.0.1:8080` | Address to bind the HTTP server |
| `--repo-path` | `MEMORY_MCP_REPO_PATH` | `~/.memory-mcp` | Path to the git-backed memory repository |
| `--embedding-model` | `MEMORY_MCP_EMBEDDING_MODEL` | `BGESmallENV15` | Embedding model name |
| `--mcp-path` | `MEMORY_MCP_PATH` | `/mcp` | URL path for the MCP endpoint |
| `--remote-url` | `MEMORY_MCP_REMOTE_URL` | *(none)* | Git remote URL. Omit for local-only mode. |
| `--branch` | `MEMORY_MCP_BRANCH` | `main` | Branch for push/pull operations |

## Authentication

For syncing with a private GitHub remote:

```bash
# Interactive OAuth device flow — opens browser, stores token in keyring
memory-mcp auth login

# Or specify storage explicitly
memory-mcp auth login --store keyring   # system keyring (default)
memory-mcp auth login --store file      # ~/.memory-mcp-token
memory-mcp auth login --store stdout    # print token, pipe to your own storage

# Kubernetes deployments (requires --features k8s)
memory-mcp auth login --store k8s-secret

# Check current auth status
memory-mcp auth status
```

Token resolution order: `MEMORY_MCP_GITHUB_TOKEN` env var → `~/.memory-mcp-token` file → system keyring.

## Embedding models

The default model (`BGESmallENV15`, 384 dimensions) is a good balance of quality and speed. All models run locally via ONNX Runtime — no API keys required.

<details>
<summary>Supported models</summary>

| Model | Dimensions | Notes |
|-------|-----------|-------|
| BGESmallENV15 | 384 | **Default.** Best speed/quality tradeoff. |
| BGESmallENV15Q | 384 | Quantised variant, faster inference. |
| BGEBaseENV15 | 768 | Higher quality, larger model. |
| BGELargeENV15 | 1024 | Highest quality BGE model. |
| AllMiniLML6V2 | 384 | Popular lightweight model. |
| AllMiniLML12V2 | 384 | 12-layer variant. |
| NomicEmbedTextV1 | 768 | Good for long documents. |
| NomicEmbedTextV15 | 768 | Updated Nomic model. |
| MultilingualE5Small | 384 | Multilingual support. |
| MultilingualE5Base | 768 | Multilingual, higher quality. |
| MultilingualE5Large | 1024 | Multilingual, highest quality. |
| MxbaiEmbedLargeV1 | 1024 | Strong general-purpose model. |
| SnowflakeArcticEmbedXS | 384 | Compact Arctic model. |
| SnowflakeArcticEmbedS | 384 | Small Arctic model. |
| SnowflakeArcticEmbedM | 768 | Medium Arctic model. |
| SnowflakeArcticEmbedL | 1024 | Large Arctic model. |

Most models also have quantised (Q) variants for faster inference with slightly lower quality. Model names are case-insensitive.

</details>

## Deployment

### Container image

```bash
# Pull from GitHub Container Registry
docker pull ghcr.io/butterflyskies/memory-mcp:latest

# Or build locally
docker build -t memory-mcp .
```

The container image:
- Uses a multi-stage build (compile → model warmup → slim runtime)
- Ships with the embedding model pre-downloaded (no internet needed at startup)
- Runs as a non-root user (`memory-mcp`, uid 1000)
- Includes SLSA provenance and SBOM attestations

### Kubernetes

Manifests are provided in `deploy/k8s/`:

```bash
kubectl apply -f deploy/k8s/namespace.yml
kubectl apply -f deploy/k8s/rbac.yml
kubectl apply -f deploy/k8s/pvc.yml
kubectl apply -f deploy/k8s/service.yml
kubectl apply -f deploy/k8s/deployment.yml
```

The deployment is hardened with:
- `readOnlyRootFilesystem`, `runAsNonRoot`, `drop: [ALL]` capabilities
- Split ServiceAccounts (runtime vs bootstrap)
- Seccomp `RuntimeDefault` profile

See [docs/deployment.md](docs/deployment.md) for the full guide.

## Architecture decisions

This project documents significant decisions as Architecture Decision Records:

| ADR | Decision |
|-----|----------|
| [0001](docs/adr/0001-streamable-http-only.md) | Streamable HTTP transport only (no stdio) |
| [0002](docs/adr/0002-git2-over-shell.md) | git2 bindings over shelling out to git CLI |
| [0003](docs/adr/0003-usearch-vector-index.md) | usearch for HNSW vector indexing |
| [0004](docs/adr/0004-no-tokens-in-cli-args.md) | No tokens in CLI arguments |
| [0005](docs/adr/0005-fastembed-configurable-from-start.md) | fastembed configurable from day one |
| [0006](docs/adr/0006-structured-observability-from-day-one.md) | Structured observability from day one |
| [0007](docs/adr/0007-recency-based-conflict-resolution.md) | Recency-based conflict resolution for sync |
| [0010](docs/adr/0010-keyring-based-token-storage.md) | Keyring-based token storage |
| [0012](docs/adr/0012-oauth-device-flow-token-acquisition.md) | OAuth device flow for authentication |
| [0014](docs/adr/0014-container-deployment-strategy.md) | Container deployment strategy |

## Security

- **Local inference**: embeddings are computed on your machine. Memory content never leaves your network unless you push to a remote.
- **Token handling**: tokens are stored in the system keyring (or Kubernetes Secrets), never in CLI arguments or git history. Process umask is set to `0o077`.
- **Input validation**: memory names, content size, and nesting depth are validated. Path traversal and symlink attacks are blocked.
- **Container hardening**: non-root user, read-only filesystem, dropped capabilities, seccomp profile.
- **Supply chain**: CI pins all GitHub Actions to commit SHAs. Container images include SLSA provenance and SBOM attestations. Dependencies are audited with `cargo audit` on every build.

## Development

```bash
# Run tests
cargo nextest run --workspace --no-fail-fast

# With Kubernetes feature
cargo nextest run --workspace --no-fail-fast --features k8s

# Lint
cargo fmt --check
cargo clippy --workspace -- -D warnings

# Audit dependencies
cargo audit
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
