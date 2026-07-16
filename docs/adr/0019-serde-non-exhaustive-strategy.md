# ADR-0019: Serde + #[non_exhaustive] strategy for public types

## Status
Accepted. ADR-0040 supersedes only this ADR's Rust API-surface consequence; the strict serde decision remains binding.

## Context
Public enums like `Scope` are `#[non_exhaustive]` for semver safety (new variants don't break downstream `match`).
However, `Scope` is also serialized to YAML frontmatter in memory files shared via git.
Adding a variant is semver-compatible at the Rust level but data-incompatible at the serde level:
older binaries will fail to deserialize the unknown variant.

Options considered:
1. `#[serde(other)]` fallback variant (`Unknown`) — graceful degradation, but silent data corruption on round-trip
2. Fail loudly, skip gracefully — strict deserialization, but operations skip unparseable files with warnings
3. Format versioning — frontmatter version field so readers can reject data they can't handle

## Decision
Option 2: keep `Scope` strict (no `Unknown` variant). Operations that iterate over memory files
(`list_memories`, `recall`) already catch per-memory parse errors and skip with warnings.
This preserves data integrity — if you can't understand it, don't touch it.

## Consequences
- Old binaries encountering new `Scope` variants will warn and skip those memories, not crash
- No risk of silent data corruption from round-tripping an `Unknown` variant
- Users see clear warnings to upgrade when new variants appear
- The former blanket `#[non_exhaustive]` consequence is superseded by ADR-0040; strict deserialization remains unchanged
- Subsumes #65 and #67 — no format versioning needed at this stage
