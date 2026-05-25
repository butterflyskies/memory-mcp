<!-- design-meta
status: approved
last-updated: 2026-05-25
phase: 3
-->

# Architecture: Memory Metadata Framework

This document captures the architecture of the memory-mcp metadata framework redesign through a set of Mermaid diagrams. Each diagram targets a distinct architectural concern: system boundaries, internal structure, data flow with trust zones, the metadata schema, and the key write path.

---

### System Context

Memory-mcp sits at the center of a three-actor system: AI agents drive the primary read/write workload via MCP tools, admins shape behavior through deployment config, and external tools access the underlying git repository directly. The MCP server mediates all structured access, while the git repo serves as the shared persistence layer that external tooling can reach without going through the server.

```mermaid
C4Context
    title System Context — memory-mcp

    Person(agent, "AI Agent", "Calls MCP tools to remember, recall, and manage memories during task execution")
    Person(admin, "Admin", "Deploys and configures the server via TOML config files")
    Person_Ext(external, "External Tools", "Obsidian, scripts, CI — read/write markdown files in the git repo directly")

    System(mcp, "memory-mcp", "Semantic memory server. Accepts MCP tool calls, validates metadata against schema, stores markdown+YAML in git, indexes embeddings for semantic search.")

    SystemDb(git, "Git Repository", "Bare or working repo containing markdown memory files. Namespace maps to directory structure.")
    SystemDb(vectors, "Vector Index", "Embedding store for semantic recall (sled or qdrant)")
    SystemDb(recalllog, "Recall Log", "SQLite-backed telemetry for mark_applied feedback loop")

    Rel(agent, mcp, "MCP tool calls (remember/recall/read/edit/forget/list/sync/mark_applied)", "stdio / SSE")
    Rel(admin, mcp, "Deployment config", "TOML file on disk")
    Rel(mcp, git, "Read/write markdown files", "git2 / libgit2")
    Rel(mcp, vectors, "Upsert and query embeddings", "internal")
    Rel(mcp, recalllog, "Write recall events and verdicts", "SQLite")
    Rel(external, git, "Direct file access", "filesystem / git")
```

---

### Component Diagram

The server is organized into four layers: the MCP interface (tool handlers), the domain model (types and validation), the infrastructure adapters (serialization, storage, indexing), and the telemetry subsystem. Each component has a narrow interface; the schema validator and serialization adapter are the main extension points for the new metadata design.

```mermaid
graph TD
    subgraph MCP_Interface["MCP Interface Layer"]
        TH["Tool Handlers<br/>(server.rs)<br/>remember · recall · read · edit<br/>forget · list · sync<br/>mark_applied · recall_stats"]
    end

    subgraph Domain["Domain Layer"]
        MM["Metadata Model<br/>(types.rs)<br/>Memory · MemoryMetadata<br/>Scope · Retention · Classification"]
        SV["Schema Validator<br/>Validates writes against<br/>deployment config schema"]
        RE["Retention Evaluator<br/>TTL filter (server-side)<br/>Condition passthrough (agent-side)"]
        PE["Policy Engine<br/>(future stub)<br/>classification + namespace + identity → allow/deny"]
    end

    subgraph Infra["Infrastructure Layer"]
        SL["Serialization Layer<br/>MemorySerializer trait<br/>YamlFrontmatterSerializer (current)<br/>JsonSerializer (future)"]
        GR["Git Repository<br/>(repo.rs)<br/>Markdown files<br/>namespace → directory path"]
        VI["Vector Index<br/>Embedding upsert + query<br/>semantic recall"]
        DC["Deployment Config<br/>(config.toml)<br/>valid classifications<br/>default retention<br/>custom field schema"]
    end

    subgraph Telemetry["Telemetry"]
        RL["Recall Log<br/>(recall_log.rs)<br/>SQLite: recall events + verdicts<br/>precision calibration"]
    end

    TH --> MM
    TH --> SV
    TH --> RE
    TH --> PE
    MM --> SL
    SL --> GR
    TH --> VI
    SV --> DC
    RE --> DC
    PE --> DC
    TH --> RL
```

---

### Data Flow with Trust Boundaries

Three trust zones are in play: the agent environment (untrusted input), the mcp server process (trusted execution), and the storage tier (trusted but externally writable). The critical boundary crossing is the agent→server interface, where all inputs are validated against the schema config before any write reaches storage. The git→external path is unmediated, so schema invariants on externally-written files are only enforced at read time.

```mermaid
flowchart LR
    subgraph AgentZone["Agent Environment (untrusted)"]
        A["AI Agent"]
    end

    subgraph ServerZone["MCP Server Process (trusted)"]
        TH["Tool Handlers"]
        SV["Schema Validator"]
        RE["Retention Evaluator"]
        SL["YamlFrontmatter\nSerializer"]
        VI["Vector Index"]
        RL["Recall Log"]
    end

    subgraph StorageZone["Storage Tier (trusted, externally writable)"]
        GR["Git Repo\n(markdown files)"]
        SQ["SQLite\n(recall log)"]
        DC["Deployment Config\n(config.toml)"]
    end

    subgraph ExternalZone["External Tools (unmediated)"]
        EX["Obsidian / scripts / CI"]
    end

    A -- "MCP tool call\n(stdio/SSE)" --> TH
    TH -- "validate metadata" --> SV
    SV -- "read schema" --> DC
    TH -- "check retention" --> RE
    RE -- "read defaults" --> DC
    TH -- "serialize" --> SL
    SL -- "write markdown" --> GR
    TH -- "upsert embedding" --> VI
    TH -- "log recall event" --> RL
    RL --> SQ

    GR -- "read at recall/read time\n(validate on ingest)" --> TH
    EX -- "direct file r/w\n(no schema enforcement)" --> GR

    style AgentZone fill:#fef3c7,stroke:#d97706
    style ServerZone fill:#dbeafe,stroke:#2563eb
    style StorageZone fill:#dcfce7,stroke:#16a34a
    style ExternalZone fill:#fce7f3,stroke:#db2777
```

---

### Data Schema

The metadata model is organized around a central `Memory` entity that composes a `MemoryMetadata` record. Metadata splits cleanly into three tiers — core (server-managed), standard (well-known optional), and custom (deployment-defined) — with `Scope`, `Retention`, and `Classification` as first-class value types. `DeploymentConfig` is the compile-time-external schema that governs validation and defaults; it is referenced by the server at startup, not embedded in individual memory records.

```mermaid
erDiagram
    MEMORY {
        string id PK "nanoid, server-assigned"
        string content "markdown body"
    }

    MEMORY_METADATA {
        string memory_id FK
        string name "human-readable identifier"
        string scope_path "e.g. /, /foo, /org/team/proj"
        datetime created_at "server-assigned"
        datetime updated_at "server-assigned"
        string[] tags "standard: optional"
        string source "standard: optional origin hint"
        string classification_label FK "standard: optional"
        json custom_fields "custom: deployment-defined"
    }

    SCOPE {
        string path PK "path-based namespace"
        string directory_path "maps to git repo path"
    }

    RETENTION {
        string variant "Ttl | Condition | Evergreen"
        duration ttl_duration "present when variant=Ttl"
        string condition_expr "present when variant=Condition"
    }

    CLASSIFICATION {
        string label PK "deployment-defined label"
        int rank "ordering for sensitivity comparison"
        string description "human-readable"
    }

    CUSTOM_FIELD_DEF {
        string name PK
        string field_type "string | integer | bool | string_list | enum"
        bool required
        string default_value "optional"
    }

    DEPLOYMENT_CONFIG {
        int schema_version "starts at 1"
        string config_path "path to config.toml"
        string default_retention "Retention variant"
        string default_classification "label"
    }

    MEMORY ||--|| MEMORY_METADATA : "has"
    MEMORY_METADATA }o--|| SCOPE : "scoped to"
    MEMORY_METADATA ||--o| RETENTION : "has retention"
    MEMORY_METADATA }o--o| CLASSIFICATION : "labeled with"
    DEPLOYMENT_CONFIG ||--o{ CLASSIFICATION : "defines valid"
    DEPLOYMENT_CONFIG ||--o{ CUSTOM_FIELD_DEF : "defines"
    MEMORY_METADATA }o--o{ CUSTOM_FIELD_DEF : "values conform to"
```

---

### Sequence: `remember` Tool Call

The `remember` flow is the primary write path and exercises all new framework components in order: schema validation (including default injection), retention evaluation, serialization to YAML frontmatter, git commit, and embedding upsert. Schema validation is a hard gate — any unknown field or type mismatch returns an error before touching storage. Default injection runs only on validated records, filling in absent optional fields from the deployment config.

```mermaid
sequenceDiagram
    actor Agent
    participant TH as Tool Handler<br/>(server.rs)
    participant SV as Schema Validator
    participant RE as Retention Evaluator
    participant DC as Deployment Config
    participant SL as YamlFrontmatterSerializer
    participant GR as Git Repository
    participant VI as Vector Index

    Agent->>TH: remember(name, content, scope?, tags?, retention?, classification?, custom?)

    TH->>DC: load schema (cached)
    DC-->>TH: CustomFieldDefs, valid_classifications, default_retention

    TH->>SV: validate(metadata, schema)
    note over SV: Check required custom fields present<br/>Check types match definitions<br/>Check classification is in valid set
    alt validation failure
        SV-->>TH: Err(ValidationError)
        TH-->>Agent: error response
    end
    SV-->>TH: Ok(validated_metadata)

    TH->>RE: apply_defaults(metadata, schema)
    note over RE: Inject default_retention if absent<br/>Inject custom field defaults if absent
    RE-->>TH: metadata_with_defaults

    TH->>TH: assign id (nanoid), set created_at / updated_at

    TH->>SL: serialize(Memory { id, content, metadata })
    note over SL: Build YAML frontmatter<br/>core fields → top-level<br/>standard fields → top-level<br/>custom fields → custom: section
    SL-->>TH: markdown_string

    TH->>GR: write_file(scope_path/name.md, markdown_string)
    GR->>GR: git add + git commit
    GR-->>TH: Ok(commit_sha)

    TH->>VI: upsert_embedding(id, content + name)
    VI-->>TH: Ok

    TH-->>Agent: MemoryRecord { id, name, scope, created_at }
```

---

### Observability: OpenTelemetry Spans

All new components emit OTel spans. This is table stakes — the existing tracing scaffold (see [tracing design](../tracing/)) established the pattern; the metadata framework extends it.

#### Spans per component

| Component | Span name | Key attributes | Emitted on |
|-----------|-----------|----------------|------------|
| Schema Validator | `schema.validate` | `field_count`, `custom_field_count`, `result` (ok/error), `error_reason` | `remember`, `edit` |
| Retention Evaluator | `retention.evaluate` | `variant` (ttl/condition/evergreen), `result` (fresh/expired/passthrough), `memories_filtered` | `recall`, `read`, `list` |
| MemorySerializer | `serialize` | `format` (yaml_frontmatter), `total_fields`, `custom_fields` | `remember`, `edit` |
| MemorySerializer | `deserialize` | `format`, `had_unknown_fields`, `applied_defaults` | `read`, `recall`, `list`, startup ingest |
| Ingest Validator | `ingest.validate` | `files_scanned`, `files_passed`, `files_skipped`, `skip_reasons` | startup, read |
| Classification Filter | `classification.filter` | `filter_applied`, `memories_excluded`, `classification_level` | `recall` |
| Audit Logger | `audit.write` | `operation`, `commit_sha`, `classification_change` (if any), `source` | `remember`, `edit`, `forget` |
| Retention Reaper | `retention.reap` | `expired_count`, `deleted_count`, `errors` | background/lazy cleanup |

#### Span hierarchy

Tool handler spans (already exist) become parents:
```
remember (existing)
  └── schema.validate
  └── retention.evaluate (apply defaults)
  └── serialize
  └── repo.write (existing)
  └── index.upsert (existing)
  └── audit.write

recall (existing)
  └── index.query (existing)
  └── deserialize (per result)
  └── retention.evaluate (per result)
  └── classification.filter
```

#### Error attributes

All spans record `otel.status_code` = `ERROR` on failure with `error.type` and `error.message`. Schema validation errors include the specific field and constraint that failed. Ingest validation errors include the file path and parse error.

---

### Implementation Notes

#### MemorySerializer: trait object, not generic

The serializer is stored as `Arc<dyn MemorySerializer + Send + Sync>` for runtime backend selection. Not generic — we expect few implementations and runtime config determines the serializer.

#### Re-embedding rules

| Change | Re-embed? | Rationale |
|--------|-----------|-----------|
| Content modified | Yes | Content participates in embedding |
| Name changed | Yes | Name participates in embedding text |
| Scope/classification/retention changed | No | Metadata-only, not in embedding text |
| Tags/source changed | No | Not included in embedding input |
| Custom fields changed | No | Not included in embedding input |

This must be enforced as a documented invariant and tested. Accidental re-embedding on metadata-only changes degrades recall quality silently.

#### Defaults: virtual, not materialized

When reading a memory that lacks retention, classification, or custom field values, the server returns the deployment defaults in the response but does **not** rewrite the frontmatter file. Materialization only occurs on explicit `edit`. This prevents silent file modification that would confuse external tools (Obsidian) and create noisy git diffs.

#### Classification: single-label, ordered

Each memory has exactly one classification label, not a list. The deployment config defines labels with explicit rank ordering. Recall filtering compares ranks: "exclude memories with rank above N." This avoids the multi-label taxonomy trap.

```toml
schema_version = 1

[classification]
default = "internal"

[[classification.labels]]
name = "public"
rank = 10

[[classification.labels]]
name = "internal"
rank = 20

[[classification.labels]]
name = "confidential"
rank = 30

[[classification.labels]]
name = "restricted"
rank = 40
```

#### Unknown field preservation (four-bucket model)

On deserialization, frontmatter fields are classified into four buckets:
1. **Core fields** — server-managed, always present
2. **Standard fields** — well-known optional (tags, source, retention, classification)
3. **Schema-known custom fields** — validated against deployment config
4. **Unknown legacy fields** — preserved on round-trip but not accepted in new writes

Bucket 4 enables backward compatibility: old memories with pre-framework fields are preserved, but new `remember` calls cannot introduce arbitrary unknown fields outside the `custom:` section.
