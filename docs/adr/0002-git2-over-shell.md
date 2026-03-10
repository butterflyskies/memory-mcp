# ADR-0002: git2 Over Shelling Out to Git CLI

## Status
Accepted

## Context
The server needs git operations (init, add, commit, push, pull) for memory persistence and sync.
Two options: the `git2` crate (libgit2 Rust bindings) or shelling out to the `git` CLI.

## Decision
Use `git2`. Prefer build-time coupling (C dependency bundled via `libgit2-sys`) over runtime
coupling (assuming `git` is installed and on PATH).

## Consequences
- No runtime dependency on git being installed — works in minimal container images
- Build requires a C compiler and CMake (libgit2-sys bundles the C source)
- git2's `Repository` is `Send` but not `Sync` — requires a `tokio::sync::Mutex` wrapper for concurrent access
- Auth handled via git2's credential callbacks rather than git's credential helper config
