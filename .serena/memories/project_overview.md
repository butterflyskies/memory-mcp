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
- **Repo ruleset** (`require CI checks`): test, build, audit, msrv, cross-compile (Linux + macOS), lint — all must pass before merge

## Trust Signals
- **Shipped**: Cargo.toml metadata (description, repository, keywords, categories incl. artificial-intelligence), cargo-deny replacing cargo-audit, `#![warn(missing_docs)]` on lib crate, MSRV 1.88 declared + CI enforced
- **ADRs**: 0017 (MSRV match-deps policy), 0018 (dedicated GitHub App for release-please)
- **Deferred**: cargo-auditable (#60, blocked on upstream action support), cargo-semver-checks (#61 prereq: lib refactor), crates.io publish (#62, blocked on #61)

## Release Infrastructure
- **GitHub App**: `butterflyskies-release-manager-bot` (app ID 3144639) generates tokens for release-please so its PRs trigger CI workflows
- **Org secrets**: `RELEASE_BOT_APP_ID`, `RELEASE_BOT_PRIVATE_KEY`
- **deny.toml**: license allowlist tuned to actual transitive deps; webpki-roots CDLA-Permissive-2.0 as scoped exception; unmaintained transitive deps (number_prefix, paste) ignored

## Status
v0.2.0 released. Trust signals Phase 1+2 shipped. Next: lib refactor (#61), then semver-checks + crates.io publish (#62). Also: BM25/Tantivy (#55), cross-platform vector index (#56), tracing/observability (#52).
