# ADR-0014: Container deployment strategy

## Status
Accepted

## Context
memory-mcp needs to run in Kubernetes. The server depends on a fastembed model (~80MB)
that takes several seconds to load. Container images must be self-contained and start fast.

## Decision
Multi-stage Dockerfile: Rust builder stage compiles with `--features k8s`, a model stage
downloads AllMiniLML6V2 at build time, and a minimal runtime stage combines both. A `warmup`
subcommand pre-loads the embedding model so readiness probes pass quickly. `/healthz` endpoint
for liveness/readiness checks. GitHub Actions builds and pushes to Harbor.

## Consequences
- Container images are ~150-200MB (static binary + model weights)
- No network fetch at startup — air-gapped friendly
- `warmup` can be used as an init container or entrypoint preamble
- GitHub Actions provides reproducible builds from the start
