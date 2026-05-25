# ADR-0028: Schema-driven extensible metadata model

## Status
Accepted

## Context
Memory-mcp's metadata is hardcoded: `MemoryMetadata` carries a fixed set of fields (tags, scope, source, timestamps), and adding a new field requires changes in 5+ locations (struct definition, two inline `Frontmatter` structs for ser/deser, tool args, server handlers). This makes the metadata model brittle and couples it to the YAML frontmatter serialization format.

We need to add retention, classification, and deployment-specific custom fields. Different deployments (personal, team, enterprise) need different metadata schemas. External tools (Obsidian) consume the frontmatter directly.

## Decision
Split metadata into three tiers:
- **Core** (server-managed, always present): id, name, scope, created_at, updated_at
- **Standard** (well-known, optional): tags, source, retention, classification
- **Custom** (deployment-defined): arbitrary fields validated against a deployment schema config

Custom fields are stored in a `custom:` subsection of YAML frontmatter to prevent collision with core/standard field names. The deployment config (TOML) defines valid custom field names, types, required/optional status, and defaults. Schema validation runs on every write; defaults are applied on read for missing fields.

## Consequences
- Adding a new standard field is a code change; adding a custom field is a config change
- Backward compatible: existing memories without new fields load with defaults
- Schema validation adds a code path to every write operation
- Custom fields are intentionally flat (string, number, bool, list) — no nested objects
