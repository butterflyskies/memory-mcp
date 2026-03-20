# memory-mcp — Project Overview

## Purpose
A semantic memory system for AI coding agents, exposed as an MCP server. Memories are stored as markdown files in a git repository, synced across devices via GitHub, and indexed for semantic retrieval using local embeddings.

**Repo**: https://github.com/butterflyskies/memory-mcp (public, MIT OR Apache-2.0)

## Tech Stack
- **Language**: Rust (edition 2021)
- **MCP framework**: rmcp (v1.1) with streamable HTTP transport via Axum
- **HTTP**: Axum 0.8
- **Git**: git2 0.20 (libgit2 bindings, no CLI shelling)
- **Embeddings**: candle direct (local BERT inference, BGE-small-en-v1.5)
- **Vector index**: usearch 2 (HNSW with cosine metric)
- **CLI**: clap with derive
- **Auth**: GitHub token via env var → keyring → token file; OAuth device flow; k8s-secret backend (feature-gated)

## Transport
Streamable HTTP only (no stdio, no SSE). Single binary serves both local dev and k8s.

## CLI Structure
- `memory-mcp serve` (default) — runs the MCP server
- `memory-mcp auth login [--store keyring|file|stdout|k8s-secret]` — OAuth device flow
- `memory-mcp auth status` — show resolved token source
- `memory-mcp warmup` — pre-download embedding model (used in Dockerfile)

## Container & CI
- **Registry**: ghcr.io/butterflyskies/memory-mcp
- **Dockerfile**: multi-stage (rust:trixie → model warmup → debian:trixie-slim runtime)
- **Trixie used**: Debian Trixie base image (no longer required by glibc constraints since fastembed/ort-sys removal)
- **HF_HOME**: HuggingFace Hub model cache directory; pinned to absolute path in Docker
- **CI**: GitHub Actions — fmt, clippy, nextest, cargo audit, Docker build, cross-platform build (Linux + macOS)
- **Cross-compile**: `cargo build --features k8s` on ubuntu-latest + macos-latest in PRs; Windows blocked by usearch (#42)
- **OpenSSL**: vendored on non-Linux (macOS/Windows lack system headers); Linux uses system OpenSSL via pkg-config
- **Attestations**: SLSA provenance + SBOM on every image push
- **Release**: release-please (draft → upload assets → undraft) with conventional commits, Node.js 24 opt-in

## Security
- Process-wide umask 0o077, atomic token writes, no silent credential fallback
- Container: non-root user, readOnlyRootFilesystem, drop ALL capabilities, seccomp RuntimeDefault
- All GHA actions pinned to commit SHAs
- cargo audit in CI pipeline

## Branch Protection
- **Org ruleset** (`protect main branch`): deletion, no force-push, linear history, signed commits, 1 approving review, Copilot code review
- **Repo ruleset** (`require CI checks`): test, build, audit, cross-compile (Linux + macOS), lint — all must pass before merge

## Status
Phase 2 mostly complete. candle direct embeddings shipped (PR #51, v0.1.6 pending release). Next: BM25/Tantivy (Phase 2 of retrieval plan), migration tools, k8s cluster deployment (ArgoCD, HTTPRoute, certs), cosign signing, CVE scanning gates.
