# memory-mcp — Project Overview

## Purpose
A semantic memory system for AI coding agents, exposed as an MCP server. Memories are stored as markdown files in a git repository, synced across devices via GitHub, and indexed for semantic retrieval using local embeddings.

**Repo**: https://github.com/butterflyskies/memory-mcp (public, MIT OR Apache-2.0)
**crates.io**: https://crates.io/crates/memory-mcp (published v0.3.0, 2026-03-21)

## Tech Stack
- **Language**: Rust (edition 2021)
- **MCP framework**: rmcp (v1.1) with streamable HTTP transport via Axum
- **HTTP**: Axum 0.8
- **Git**: git2 0.20 (libgit2 bindings, no CLI shelling)
- **Embeddings**: candle direct (local BERT inference, BGE-small-en-v1.5)
- **Vector index**: usearch 2 (HNSW with cosine metric)
- **CLI**: clap with derive
- **Auth**: GitHub token via env var → keyring → token file; OAuth device flow; k8s-secret backend (feature-gated)
- **Credentials**: secrecy crate (SecretString, zeroize-on-drop)
- **Path resolution**: homedir + shellexpand

## Transport
Streamable HTTP only (no stdio, no SSE). Single binary serves both local dev and k8s.

## CLI Structure
- `memory-mcp serve` (default) — runs the MCP server
- `memory-mcp auth login [--store keyring|file|stdout|k8s-secret]` — OAuth device flow
- `memory-mcp auth status` — show resolved token source and provenance
- `memory-mcp warmup` — pre-download embedding model (used in Dockerfile)

## Container & CI
- **Registry**: ghcr.io/butterflyskies/memory-mcp
- **Dockerfile**: multi-stage (rust:trixie → model warmup → debian:trixie-slim runtime)
- **CI**: GitHub Actions — fmt, clippy, nextest, cargo-deny, cargo-semver-checks, Docker build, cross-platform build (Linux + macOS)
- **Cross-compile**: `cargo build --features k8s` on ubuntu-latest + macos-latest; Windows blocked by usearch (#42)
- **Attestations**: SLSA provenance + SBOM on every image push
- **Release**: release-please with conventional commits

## Security
- Process-wide umask 0o077, no silent credential fallback
- SecretString (secrecy crate) for all token handling — zeroize-on-drop
- Container: non-root user, readOnlyRootFilesystem, drop ALL caps, seccomp RuntimeDefault
- All GHA actions pinned to commit SHAs
- cargo-deny in CI pipeline

## Branch Protection
- **Org ruleset**: deletion, no force-push, linear history, signed commits, 1 review, Copilot review
- **Repo ruleset**: test, build, audit, msrv, semver-checks, cross-compile (Linux + macOS), lint

## Status
v0.3.0 published to crates.io. Trust signals Phase 2.5 complete. Next: k8s deployment workstream (see docs/deployment-workstream.md), then Serena memory migration.
