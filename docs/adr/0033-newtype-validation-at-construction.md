# ADR-0033: Newtype validation at construction, not at call sites

## Status
Accepted

## Context
Domain identifiers like memory names and scope+name pairs were passed as bare `String` and
`(Scope, String)` tuples throughout the codebase. Validation happened at every function
boundary via scattered `validate_name()` calls — 8+ in server.rs and repo.rs. This meant:
- Forgetting a validation call was a silent bug
- Internal code couldn't assume a name was already valid
- Refactoring required auditing every call site

## Decision
Introduce validated newtypes (`MemoryName`, `MemoryRef`) that enforce invariants at
construction. Once you hold a `MemoryName`, it is guaranteed valid — no re-checking needed.
JSON deserialization still accepts `String` at the API boundary; conversion to `MemoryName`
happens once in the handler. `MemoryRef` pairs a `Scope` with a `MemoryName` and provides
`qualified_path()`, replacing ad-hoc `format!()` calls.

This is part of a deliberate "TMFDYUT" (newtypes) pass across the codebase.

## Consequences
- Validation is impossible to skip — the type system enforces it
- Internal functions accept newtypes, eliminating redundant checks
- API boundary unchanged — serde still deserializes strings, conversion is explicit
- `from_validated()` constructors allow `pub(crate)` fast paths where validity is known

## References
- #224 (MemoryName), #228 (MemoryRef)
