# memory-mcp — Project Overview

## Purpose
A semantic memory system for AI coding agents, exposed as an MCP server. Memories are stored as markdown files in a git repository, synced across devices via a private GitHub remote, and indexed for semantic retrieval using local embeddings.

## Tech Stack
- **Language**: Rust
- **MCP framework**: rmcp (v1.1) with streamable HTTP transport via Axum
- **HTTP**: Axum 0.8
- **Git**: git2 0.20 (libgit2 bindings, no CLI shelling)
- **Embeddings**: fastembed (local model, default AllMiniLML6V2)
- **Vector index**: usearch (HNSW with cosine metric)
- **Serialization**: serde, serde_json, serde_yaml
- **CLI**: clap with derive
- **Error handling**: thiserror (library), anyhow (application)
- **Logging**: tracing + tracing-subscriber (stderr only)
- **Auth**: GitHub token via env var → keyring → token file fallback chain; OAuth device flow for acquisition (`auth login`)

## Transport
Streamable HTTP only (no stdio, no SSE). Single binary serves both local dev and k8s.

## MCP Tools (7)
1. `remember(content, tags?, scope?)` — store new memory
2. `recall(query, scope?, limit?)` — semantic search
3. `forget(name_or_query)` — delete memory
4. `edit(name, content?, tags?)` — modify existing memory
5. `list(scope?)` — browse memories
6. `read(name)` — read specific memory
7. `sync()` — git push/pull

## CLI Structure
- `memory-mcp serve` (default) — runs the MCP server
- `memory-mcp auth login [--store keyring|file|stdout]` — OAuth device flow
- `memory-mcp auth status` — show resolved token source

## Security
- Process-wide umask 0o077
- Atomic token file writes (write-to-temp-then-rename with fsync)
- No silent fallback to plaintext file storage for credentials
- reqwest with explicit rustls-tls, connect/read timeouts
- OAuth polling: expires_in capped at 1800s, interval clamped

## Status
Phase 2 in progress. Core server functional with all 7 tools, git sync with incremental reindex, keyring auth, OAuth device flow. Next: --store k8s-secret, migration tools, k8s deployment.
