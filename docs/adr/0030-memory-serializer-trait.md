# ADR-0030: MemorySerializer trait separating model from format

## Status
Accepted

## Context
The metadata model is currently fused with its serialization format. `Memory::to_markdown()` and `Memory::from_markdown()` contain hardcoded `Frontmatter` structs that define both the logical fields and their YAML representation. This means:
- A database backend would need entirely different code despite storing the same logical data
- The YAML format can't evolve independently of the data model
- Obsidian compatibility is tangled with business logic

## Decision
Extract a `MemorySerializer` trait:
```rust
trait MemorySerializer {
    fn serialize(&self, memory: &Memory) -> Result<Vec<u8>>;
    fn deserialize(&self, bytes: &[u8]) -> Result<Memory>;
}
```

`YamlFrontmatterSerializer` is the current (and only) implementation. The existing `to_markdown()`/`from_markdown()` methods become thin wrappers. Future backends (JSON for databases, alternative frontmatter formats) implement the same trait.

The serializer is responsible for structural safety: custom field values are always double-quoted YAML scalars regardless of content (defense against YAML injection).

## Consequences
- One indirection layer between storage and model
- Serializer owns format-specific safety (quoting, escaping) — not the caller's responsibility
- Obsidian compatibility becomes a serializer concern, not a model concern
- Future database backends slot in without touching the domain layer
