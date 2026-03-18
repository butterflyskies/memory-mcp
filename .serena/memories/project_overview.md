# memory-mcp — Project Overview

## Purpose
A semantic memory system for AI coding agents, exposed as an MCP server. Memories are stored as markdown files in a git repository, synced across devices via GitHub, and indexed for semantic retrieval using local embeddings.

**Repo**: https://github.com/butterflyskies/memory-mcp (public, MIT OR Apache-2.0)

## Tech Stack
- **Language**: Rust (edition 2021)
- **MCP framework**: rmcp (v1.1) with streamable HTTP transport via Axum
- **HTTP**: Axum 0.8
- **Git**: git2 0.20 (libgit2 bindings, no CLI shelling)
- **Embeddings**: fastembed 5 (local model, default BGESmallENV15)
- **Vector index**: usearch 2 (HNSW with cosine metric)
- **CLI**: clap with derive
- **Auth**: GitHub token via env var → keyring → token file; OAuth device flow; k8s-secret backend (feature-gated)

## Transport
Streamable HTTP only (no stdio, no SSE). Single binary serves both local dev and k8s.

## CLI Structure
- `memory-mcp serve` (default) — runs the MCP server
- `memory-mcp auth login [--store keyring|file|stdout|k8s-secret]` — OAuth device flow
- `memory-mcp auth status` — show resolved token source
- `memory-mcp warmup [--embedding-model]` — pre-download embedding model (used in Dockerfile)

## Container & CI
- **Registry**: ghcr.io/butterflyskies/memory-mcp
- **Dockerfile**: multi-stage (rust:trixie → model warmup → debian:trixie-slim runtime)
- **Trixie required**: ort_sys (ONNX Runtime) needs glibc ≥2.38; Bookworm only has 2.36
- **FASTEMBED_CACHE_DIR**: must be pinned to absolute path in Docker (defaults to CWD-relative `.fastembed_cache`)
- **CI**: GitHub Actions — fmt, clippy, nextest, cargo audit, Docker build
- **Attestations**: SLSA provenance + SBOM on every image push
- **Release**: release-please with conventional commit enforcement (lint-pr workflow)

## Security
- Process-wide umask 0o077, atomic token writes, no silent credential fallback
- Container: non-root user, readOnlyRootFilesystem, drop ALL capabilities, seccomp RuntimeDefault
- All GHA actions pinned to commit SHAs
- cargo audit in CI pipeline

## Status
Phase 2 mostly complete. Next: migration tools, k8s cluster deployment (ArgoCD, HTTPRoute, certs), cosign signing, CVE scanning gates.
