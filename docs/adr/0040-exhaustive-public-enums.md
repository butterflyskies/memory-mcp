# ADR-0040: Public enums are exhaustive by default

## Status

Accepted

Supersedes the `#[non_exhaustive]` default in ADR-0019.

## Context

`#[non_exhaustive]` lets a library add enum variants without a Rust semver break, but downstream consumers must use a wildcard match. That wildcard also removes the compiler's guarantee that consumers have handled every state the interface defines.

For memory-mcp's public contracts, a new enum variant is an interface change. Downstream exhaustive matches should fail to compile so consumers must make an explicit decision about the new state.

The former repository-wide rule applied `#[non_exhaustive]` mechanically to every public enum and fielded struct. That erased the distinction between APIs that deliberately promise forward-compatible matching and APIs that deliberately require exhaustive handling.

## Decision

- Public enums are exhaustive by default.
- Adding a public enum variant is an intentional breaking change and requires the corresponding version transition and migration guidance.
- `#[non_exhaustive]` may be used only when a specific public API deliberately chooses forward-compatible matching and documents why wildcard handling is correct for its consumers.
- Public structs are evaluated separately according to their construction and compatibility contract; there is no blanket attribute rule.
- Existing public types that already carry `#[non_exhaustive]` keep their published behavior until a deliberate breaking change removes it.

## Consequences

- Downstream exhaustive matches provide a compile-time inventory when an interface changes.
- Interface evolution may require more major-version changes, by design.
- Reviewers must reason from the concrete API contract instead of enforcing a universal attribute convention.
- ADR-0019 remains binding for strict serde compatibility; only its blanket Rust API-surface consequence no longer governs new types.
