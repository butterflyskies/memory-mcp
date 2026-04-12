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

### Install from crates.io

```bash
cargo install memory-mcp
```

### Or from source

```bash
git clone https://github.com/butterflyskies/memory-mcp.git
cd memory-mcp
cargo build --release
```

### Run the server

On first run, the embedding model (~130MB) is downloaded from HuggingFace Hub.
You can pre-download it with `memory-mcp warmup`.

```bash
# Starts on 127.0.0.1:8080 with a local git repo at ~/.memory-mcp
memory-mcp serve

# Or configure via environment variables
MEMORY_MCP_BIND=0.0.0.0:9090 \
MEMORY_MCP_REPO_PATH=/path/to/memories \
memory-mcp serve
```

### Connect your editor

memory-mcp uses [Streamable HTTP](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports) transport. Most MCP clients support it natively.

<details>
<summary><strong>Claude Code</strong></summary>

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

</details>

<details>
<summary><strong>Cursor</strong></summary>

Add to `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global):

```json
{
  "mcpServers": {
    "memory": {
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

</details>

<details>
<summary><strong>VS Code (GitHub Copilot)</strong></summary>

Add to `.vscode/mcp.json` in your workspace:

```json
{
  "servers": {
    "memory": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

Note: VS Code uses `"servers"` as the root key, not `"mcpServers"`.

</details>

<details>
<summary><strong>Windsurf</strong></summary>

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "memory": {
      "serverUrl": "http://localhost:8080/mcp"
    }
  }
}
```

Note: Windsurf uses `"serverUrl"`, not `"url"`.

</details>

<details>
<summary><strong>Continue.dev</strong></summary>

Add to `.continue/mcpServers/memory.yaml`:

```yaml
mcpServers:
  - name: memory
    type: streamable-http
    url: http://localhost:8080/mcp
```

</details>

<details>
<summary><strong>Claude Desktop</strong></summary>

Add via **Settings > Connectors > Add custom server** with URL `http://localhost:8080/mcp`.

Alternatively, use `mcp-remote` as a stdio bridge in `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "memory": {
      "command": "npx",
      "args": ["mcp-remote", "http://localhost:8080/mcp"]
    }
  }
}
```

</details>

<details>
<summary><strong>Zed</strong></summary>

Zed does not yet support Streamable HTTP natively. Use `mcp-remote` as a stdio bridge in `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "memory": {
      "source": "custom",
      "command": "npx",
      "args": ["mcp-remote", "http://localhost:8080/mcp"]
    }
  }
}
```

</details>

The agent can now use `remember`, `recall`, `read`, `edit`, `forget`, `list`, and `sync` as tools.

### Docker

If you have Docker installed, the container image is the fastest way to get running — the embedding model is pre-baked so there's no download delay on first start:

```bash
docker run -d --name memory-mcp \
  -p 8080:8080 \
  -v ~/.memory-mcp:/data/repo \
  ghcr.io/butterflyskies/memory-mcp:latest
```

The `-v` volume mount is required for persistence. Without it, memories are lost when the container stops.

To sync memories across devices, initialize a git remote inside the mounted repo:

```bash
cd ~/.memory-mcp
git init && git remote add origin git@github.com:you/my-memories.git
```

Then use the `sync` tool from your editor, or call `memory-mcp sync` directly.

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
Agent ──MCP──▶ memory-mcp ──▶ candle (local BERT embeddings)
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
2. **Embeddings**: content is embedded locally using [candle](https://github.com/huggingface/candle) with a BERT model — no external API calls
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
memory-mcp auth login --store file      # ~/.config/memory-mcp/token
memory-mcp auth login --store stdout    # print token, pipe to your own storage

# Kubernetes deployments (requires --features k8s)
memory-mcp auth login --store k8s-secret

# Check current auth status
memory-mcp auth status
```

Token resolution order: `MEMORY_MCP_GITHUB_TOKEN` env var → `~/.config/memory-mcp/token` file → system keyring.

## Embedding model

Embeddings are computed locally using [candle](https://github.com/huggingface/candle) with [BGE-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5) (384 dimensions). The model is downloaded from HuggingFace Hub on first run — no API keys required. Use `memory-mcp warmup` to pre-download.

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

Significant design decisions are documented as Architecture Decision Records in [`docs/adr/`](docs/adr/). Each ADR captures the context, decision, and consequences of a choice — giving future contributors the "why" behind the codebase.

## Security

- **Local inference**: embeddings are computed on your machine. Memory content never leaves your network unless you push to a remote.
- **Token handling**: tokens are stored in the system keyring (or Kubernetes Secrets), never in CLI arguments or git history. Process umask is set to `0o077`.
- **Input validation**: memory names, content size, and nesting depth are validated. Path traversal and symlink attacks are blocked.
- **Container hardening**: non-root user, read-only filesystem, dropped capabilities, seccomp profile.
- **Supply chain**: CI pins all GitHub Actions to commit SHAs. Container images include SLSA provenance and SBOM attestations. Dependencies are audited with `cargo audit` on every build.

## Roadmap

The core memory engine is stable — store, search, sync, and authenticate all work today. Planned next:

- **BM25 keyword search** alongside semantic search ([#55](https://github.com/butterflyskies/memory-mcp/issues/55))
- **Cross-platform vector index** with brute-force fallback for Windows ([#56](https://github.com/butterflyskies/memory-mcp/issues/56))
- **Deduplication** on remember (semantic similarity threshold)
- **Tag-based filtering** in recall
- **Richer observability** with structured tracing across all subsystems ([#52](https://github.com/butterflyskies/memory-mcp/issues/52))

See [TODO.md](TODO.md) for the full plan and [open issues](https://github.com/butterflyskies/memory-mcp/issues) for what's in flight.

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
