# ADR-0037: Move tool — single-commit repo primitive

## Status
Accepted

## Context
Issue #267 adds a `move` tool to relocate memories between scopes. The
operation touches two files (write destination, delete source) and the
vector index. We needed to decide whether to compose existing repo
primitives (`save_memory` + `delete_memory`) or add a dedicated method.

## Decision
A dedicated `MemoryRepo::move_memory` method writes the destination,
deletes the source, stages both changes, and commits — all in one git
commit. If any step fails, `checkout_head --force` resets the working
tree and index back to HEAD, leaving no dirty or untracked files.

Embedding and index operations remain at the handler layer — they are
not git concerns and don't benefit from the single-commit guarantee.

## Consequences
- **Single commit**: the git history shows one `chore: move memory
  'X' → 'Y'` commit instead of a save followed by a delete.
- **Clean failure**: on error, `checkout_head --force` restores the
  working tree to the last good commit. No dirty files, no untracked
  files, no partial state.
