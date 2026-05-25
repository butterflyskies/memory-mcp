<!-- design-meta
status: approved
last-updated: 2026-05-25
phase: 1
-->

# Problem Space: Memory Metadata Framework

## Issues

- [#107](https://github.com/butterflyskies/memory-mcp/issues/107) — Memory expiry: TTL and completion-triggered deletion
- [#116](https://github.com/butterflyskies/memory-mcp/issues/116) — Memory scope isolation for multi-user deployments
- [#119](https://github.com/butterflyskies/memory-mcp/issues/119) — Enterprise memory scope hierarchy
- [#248](https://github.com/butterflyskies/memory-mcp/issues/248) — Information classification metadata
- [#149](https://github.com/butterflyskies/memory-mcp/issues/149) — Memory metadata enrichment (last-accessed, access count, confidence)

## What problem exists today?

Memory-mcp's metadata model is flat and minimal. A memory carries: name, scope (`Global` or `Project(String)`), tags (freeform strings), source (optional string), timestamps, and content. This creates three concrete gaps:

### 1. Scopes are rigid

The scope model supports exactly two variants: `Global` and `Project(String)`. This can't express hierarchies (org > team > project > individual), and users already work around it with naming conventions (`project:person-<name>` is not a real scope type — it's a string that happens to look like one). The `ScopeFilter` enum (`GlobalOnly`, `ProjectAndGlobal`, `All`) further constrains query patterns.

### 2. Memories have no lifespan

Every memory is implicitly evergreen. A session handoff note and a persona definition are stored identically — both persist until manually deleted. There is no way to express at write time that a memory should expire after a duration, be deleted when a condition is met, or be explicitly marked as permanent. The context for making retention decisions is richest at creation time and degrades from there.

### 3. Memories have no sensitivity level

A summary of a private DM and a public architecture note are indistinguishable in the store. Privacy enforcement depends entirely on the agent remembering provenance across sessions and compactions — which it cannot reliably do. In multi-agent deployments, there is no mechanism to restrict which agents can recall which memories based on sensitivity.

### Meta-problem: the metadata model is hardcoded

Adding any new metadata field requires code changes in multiple locations:

1. `MemoryMetadata` struct (types.rs:258)
2. Inline `Frontmatter` struct in `to_markdown()` (types.rs:344)
3. Inline `Frontmatter` struct in `from_markdown()` (types.rs:390)
4. Tool argument structs (`RememberArgs`, `EditArgs`, etc.)
5. Server handlers in server.rs
6. Tool descriptions

This makes the metadata model brittle and couples it to the serialization format (YAML frontmatter). A database backend would need entirely different serialization code despite storing the same logical metadata.

## Who experiences this?

| Actor | Pain |
|-------|------|
| **Agents** | Can't filter by sensitivity at recall time. Can't express retention intent at write time. Accumulate stale memories that pollute recall results. |
| **Operators/admins** | Can't enforce classification policies. Can't audit sensitivity levels. Can't define org-specific metadata schemas. |
| **External tool users** (Obsidian, scripts) | Want rich, well-structured frontmatter. Currently limited by whatever fields we hardcode. |
| **Multi-agent deployments** | No mechanism to scope memory visibility by classification. |
| **memory-mcp developers** | Adding a metadata field requires touching 5+ files. No extension path for deployment-specific fields. |

## Inputs and outputs

### At write time (`remember`, `edit`)

**Current:** content, name, tags, scope, source
**Proposed:** + retention policy, classification label(s), custom metadata fields per org schema

### At read/recall time (`recall`, `read`, `list`)

**Current:** query, scope filter, limit
**Proposed:** + classification filter, retention evaluation (expired? condition met?), custom field filtering

### Transformations

- Server evaluates TTL-based retention mechanically — expired memories are excluded from results
- Agent evaluates condition-based retention at read time — the server stores the condition as opaque metadata, the agent decides whether to call `forget`
- Classification filtering can be server-enforced (if policy is configured) or agent-enforced (if policy is advisory)

## Architecture layers (separation of concerns)

The design must separate three layers that are currently fused:

1. **Metadata model** (abstract) — the schema defining what fields a memory can carry, their types, and validation rules. Backend-agnostic.
2. **Serialization format** — how the metadata is rendered for a specific storage backend. YAML frontmatter for git/markdown files, JSON columns for a database, etc.
3. **Policy engine** — what the metadata *means* at runtime. Retention evaluation, classification filtering, scope access rules. Configurable per deployment.

Currently layers 1 and 2 are fused: `MemoryMetadata` is both the data model AND the YAML frontmatter structure. Separating them enables:
- Database backends without touching the model
- Obsidian-compatible frontmatter as a serializer concern
- Deployment-specific fields without code changes

## Boundaries

### In scope

- Extensible metadata model with core + custom fields
- Schema-driven validation (configurable per deployment)
- Retention metadata: TTL, condition-based, evergreen
- Classification metadata: user-defined labels with configurable policies
- Scope model evolution toward richer namespacing
- Migration strategy for existing memories (lazy, backward-compatible)
- Separation of metadata model from serialization format

### Out of scope (for now)

- Authentication/authorization framework (#115) — classification informs access control but the auth layer is a separate feature
- Full multi-tenant RBAC — we're evolving the scope model but not building tenant isolation
- Tag-based filtering in recall (#148) — related but orthogonal
- Database backends — we design for them but implement git/markdown only
- Obsidian-specific fixes — the serializer should be clean enough, but dedicated Obsidian testing is deferred

### Adjacent systems

- **Git repository** (repo.rs) — on-disk format changes affect every consumer of the markdown files
- **Vector index** — re-embedding needed if scope representation changes affect content
- **Recall log / SQLite** (recall_log.rs) — may want to log classification/retention for analytics
- **MCP tool interface** — API surface changes (tool args, descriptions, responses)
- **External consumers** — any tool reading the git repo directly (Obsidian, scripts, other agents)

## Success criteria

1. Existing memories load and work without manual migration
2. New memories can carry retention, classification, and custom metadata
3. Server enforces TTL mechanically; agents evaluate conditions at read time
4. Classification policies are configurable per deployment, not hardcoded
5. Frontmatter remains valid YAML and compatible with Obsidian-style tools
6. Adding a new metadata field does not require code changes to the server (schema-driven)
7. The metadata model is independent of the serialization format

## Failure modes

- **Over-engineering:** building a generic metadata framework that's harder to use than hardcoded fields for the 90% case
- **Migration pain:** existing memories break or require a big-bang migration
- **Schema complexity:** operators need a PhD to configure their metadata schema
- **Performance:** schema validation on every read/write adds latency to the hot path
- **Obsidian incompatibility:** frontmatter changes break external tool consumers
