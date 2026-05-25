<!-- design-meta
status: approved
last-updated: 2026-05-25
phase: 2
-->

# Requirements: Memory Metadata Framework

## Use Cases

### Actors

| Actor | Description |
|-------|-------------|
| **Agent** | AI agent using MCP tools to store, recall, and manage memories |
| **Admin** | Human operator configuring the deployment (schema, policies, retention rules) |
| **Server** | memory-mcp process enforcing policies mechanically |
| **External tool** | Software reading the git repo directly (Obsidian, scripts, other agents) |

### Normal Use Cases

| ID | Actor | Use Case | Priority |
|----|-------|----------|----------|
| UC-01 | Agent | Store a memory with retention policy (TTL, condition, or evergreen) | Must |
| UC-02 | Agent | Store a memory with classification label(s) | Must |
| UC-03 | Agent | Store a memory with custom metadata fields defined by deployment schema | Should |
| UC-04 | Agent | Recall memories with classification filter (exclude above a sensitivity level) | Should |
| UC-05 | Agent | Read a memory and evaluate its retention condition, forget if met | Must |
| UC-06 | Agent | Edit a memory's retention, classification, or custom metadata | Must |
| UC-07 | Agent | Store a memory with a hierarchical namespace scope (e.g. `org/team/project`) | Must |
| UC-08 | Agent | Recall memories across namespace subtrees (e.g. all of `engineering/*`) | Should |
| UC-09 | Admin | Define a metadata schema for the deployment (required fields, valid classifications, field types) | Must |
| UC-10 | Admin | Define retention policies per classification level (e.g. "confidential memories expire after 90 days") | Should |
| UC-11 | Admin | Define access policies per namespace/classification combination | Should |
| UC-12 | Admin | Set default values for classification and retention when not specified by agent | Must |
| UC-13 | Server | Exclude TTL-expired memories from recall/read/list results | Must |
| UC-14 | Server | Validate metadata against deployment schema on remember/edit | Must |
| UC-15 | Server | Return retention and classification metadata in recall/read/list responses | Must |
| UC-16 | External tool | Read memory files with well-formed YAML frontmatter containing all metadata | Must |
| UC-17 | External tool | Navigate memories using frontmatter fields (tags, classification, scope) | Should |

### Abuse Cases

| ID | Actor | Abuse Case | Priority |
|----|-------|------------|----------|
| AC-01 | Agent | Store sensitive content with no classification, bypassing access controls | Must-mitigate |
| AC-02 | Agent | Set retention to "evergreen" on ephemeral data to avoid cleanup | Should-mitigate |
| AC-03 | Agent | Use custom metadata fields to inject malicious YAML that breaks frontmatter parsing | Must-mitigate |
| AC-04 | Attacker | Read classified memories by querying a less-restricted namespace | Must-mitigate |
| AC-05 | Attacker | Modify memory files on disk to escalate classification or remove retention | Should-mitigate |

### Security Use Cases

| ID | Actor | Use Case | Priority |
|----|-------|----------|----------|
| SC-01 | Server | Validate all metadata field values against schema before persisting | Must |
| SC-02 | Server | Enforce classification-based filtering when access policy is configured | Must |
| SC-03 | Server | Prevent namespace traversal (agent in `team-a/*` can't read `team-b/*` without policy) | Must |
| SC-04 | Server | Sanitize custom metadata values to prevent YAML injection | Must |
| SC-05 | Server | Log metadata changes (classification downgrades, retention overrides) for audit | Should |

## Requirements

### Metadata Model

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-01 | The metadata model shall define core fields (name, scope, timestamps, id) that are always present and server-managed | UC-15, UC-16 | Must |
| R-02 | The metadata model shall define standard optional fields (tags, source, retention, classification) with well-known semantics | UC-01, UC-02, UC-05 | Must |
| R-03 | The metadata model shall support custom fields defined by a deployment-level schema | UC-03, UC-09 | Must |
| R-04 | The metadata model shall be independent of the serialization format (not coupled to YAML frontmatter) | Problem statement | Must |
| R-05 | Custom field values shall be validated against the deployment schema on write operations | SC-01, AC-03 | Must |
| R-06 | Unknown fields in existing memories (pre-migration) shall be preserved, not rejected | UC-16 | Must |

### Namespace (Scope Evolution)

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-10 | Scopes shall support path-based namespaces (e.g. `org/team/project`) | UC-07 | Must |
| R-11 | The existing `global` and `project:<name>` scope formats shall continue to parse and function, including the behavior that project-scoped recall includes global memories | Migration | Must |
| R-12 | Recall/list shall support subtree queries (e.g. `engineering/*` matches `engineering/ml` and `engineering/infra`) | UC-08 | Should |
| R-13 | Namespace paths shall be validated to prevent traversal attacks (no `..`, no absolute paths) | SC-03, AC-04 | Must |
| R-14 | Scope shall be purely organizational (namespace) — access control is a policy layer concern, not a scope property. Namespaces organize recall; they are not a security boundary unless paired with policy enforcement and repository ACLs. | Problem statement | Must |

### Retention

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-20 | Memories shall support a TTL-based retention policy (expires after a duration from creation) | UC-01, #107 | Must |
| R-21 | Memories shall support condition-based retention (opaque string evaluated by the agent at read time) | UC-01, UC-05 | Must |
| R-22 | Memories shall support an explicit "evergreen" retention marker indicating no automatic expiry | UC-01 | Must |
| R-23 | Memories without retention metadata shall inherit the deployment's default retention policy | UC-12 | Must |
| R-24 | The server shall exclude TTL-expired memories from recall, read, and list results (returning not-found, not an expired status) | UC-13, Review | Must |
| R-25 | TTL-expired memories shall be eventually deleted from the git repo (lazy or background reaper) | UC-13 | Should |
| R-26 | Condition-based retention shall be returned to the agent as metadata — the server stores but does not evaluate conditions. Server instructions shall nudge agents to evaluate conditions on read and call `forget` when met. | UC-05, Review | Must |
| R-27 | `created_at` is server-managed and authoritative for TTL computation. External edits to `created_at` shall not extend TTL if a server-known record exists. | Review | Must |

### Classification

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-30 | Each memory shall have exactly one classification label (single-valued, not multi-label) | UC-02, #248, Review | Must |
| R-31 | Valid classification labels and their ordering (rank) shall be defined by the deployment schema, not hardcoded | UC-09, Review | Must |
| R-32 | Memories without classification shall receive the deployment's default classification | UC-12, AC-01 | Must |
| R-33 | Recall shall support an optional classification filter to exclude memories above a sensitivity rank | UC-04 | Should |
| R-34 | Classification-based access policies shall be configurable per deployment (which agents/roles can recall which classifications) | UC-11, SC-02 | Should |
| R-35 | Classification downgrades (e.g. confidential -> public) shall be logged for audit | SC-05 | Should |

### Schema & Configuration

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-40 | Deployment configuration shall define: valid classification labels, default classification, default retention, custom field definitions | UC-09, UC-10, UC-12 | Must |
| R-41 | Custom field definitions shall specify: field name, type (string, integer, bool, string list, enum — no float, no nested objects), required/optional, default value | UC-03, UC-09, Review | Must |
| R-45 | Deployment config shall include a `schema_version` field (starting at 1) to support future migration | Review | Must |
| R-42 | Schema validation shall run on `remember` and `edit` operations | SC-01 | Must |
| R-43 | Schema changes shall not invalidate existing memories — new required fields must have defaults | Migration | Must |
| R-44 | The schema configuration format shall be declarative (TOML, YAML, or similar) | UC-09 | Must |

### Serialization

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-50 | The YAML frontmatter serializer shall render all metadata fields (core, standard, custom) | UC-16 | Must |
| R-51 | Frontmatter shall remain valid YAML parseable by standard YAML libraries | UC-16 | Must |
| R-52 | The serializer shall be a separate component from the metadata model (adapter pattern) | R-04 | Must |
| R-53 | Custom metadata fields shall appear in a dedicated frontmatter section to avoid collision with core fields | AC-03, UC-17 | Should |
| R-54 | The deserializer shall handle memories written before the metadata framework (backward compatibility) | R-06 | Must |

### Migration

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-60 | Existing memories without retention/classification/custom metadata shall load without error | Migration | Must |
| R-61 | Missing standard fields shall be populated with deployment defaults virtually at read time (response shows effective values, but frontmatter is not rewritten). Materialization only occurs on explicit `edit`. | UC-12, Review | Must |
| R-62 | Agents and admins may enrich existing memories by editing metadata at any time | UC-06 | Must |
| R-63 | A bulk migration tool is not required but may be provided as a convenience | Migration | Could |

### Threat Model Requirements (from Phase 4)

| ID | Requirement | Source | Priority |
|----|-------------|--------|----------|
| R-70 | The server shall record expected classification in git commit metadata and verify on read; mismatch logged as tamper event | T-01 | Should |
| R-71 | Retention reaper shall evaluate against deployment config defaults, not frontmatter alone; deployment policy takes precedence over more-permissive frontmatter | T-02 | Must |
| R-72 | Deserializer shall treat externally-written files as adversarial: catch YAML errors per-file, never halt namespace traversal, log and skip malformed files | T-03 | Must |
| R-73 | Scope path validation (canonicalization, `..` rejection) shall run at both agent write time AND server read/ingest time independently | T-04, T-16 | Must |
| R-74 | Audit log entries for write operations shall include the git commit SHA | T-05 | Must |
| R-75 | Documentation shall state that classification labels provide no filesystem-level access control; deployments must apply repo ACLs independently | T-06 | Must |
| R-76 | Server shall enforce configurable limits at ingest: max file size (default 1MB), max namespace depth (default 10 components) | T-07 | Should |
| R-77 | Documentation shall state that external tool boundary has no policy enforcement; namespace isolation requires git-layer controls | T-08 | Must |
| R-78 | YAML serializer shall always emit custom field string values as double-quoted scalars with proper escaping; block scalars and bare scalars forbidden for custom values | T-09, T-10 | Must |
| R-79 | Schema loader shall reject custom field names that collide with core or standard field names; checked at server startup | T-11 | Must |
| R-80 | Schema validation shall enforce max byte length on string fields (default 4096) and max element count on list fields (default 100); nested object types forbidden | T-12, T-13 | Must |
| R-81 | Classification downgrade audit log writes shall be durable before git write commits; audit failure fails the operation. Audit rows shall include: operation intent, old classification, new classification, git commit SHA (nullable), outcome (started/committed/failed), and error if failed. | T-14, Review | Must |
| R-82 | (Future) Policy engine shall return identical responses for "access denied" and "no results" to prevent namespace probing | T-15 | Could |
| R-83 | Pending #115, server shall record agent-supplied source field in audit log for all writes; documented as non-cryptographic provenance | T-17 | Should |

## ASVS Review

| Category | Applicable? | Notes |
|----------|-------------|-------|
| V1: Architecture, design, threat modeling | Yes | Trust boundaries between agent/server, namespace isolation |
| V4: Access control | Yes | Classification-based filtering, namespace access policies |
| V5: Validation, sanitization, encoding | Yes | Schema validation, YAML injection prevention in custom fields |
| V7: Error handling, logging | Yes | Audit logging for classification changes |
| V8: Data protection | Yes | Classification is literally about data sensitivity |
| V12: Files and resources | Yes | Frontmatter format, file naming, git repo integrity |
| V2: Authentication | Deferred | Auth framework is #115, out of scope here |
| V3: Session management | No | Not applicable — stateless MCP tools |
| V6: Stored cryptography | No | No encryption at rest in current scope |
| V9: Communication | No | Transport security is out of scope |
| V10: Malicious code | No | Supply chain is out of scope |
| V11: Business logic | Partial | Retention evaluation logic, but straightforward |
| V13: API and web services | Partial | MCP tool interface changes |
| V14: Configuration | Yes | Schema/policy configuration must be validated |

## Security Requirements Traceability Matrix (SRTM)

| Req ID | Requirement | Source UC | ASVS | Test Case |
|--------|-------------|-----------|------|-----------|
| R-05 | Custom field values validated against schema | SC-01, AC-03 | V5 | TC-01: invalid field value rejected |
| R-06 | Unknown fields in pre-migration memories preserved | UC-16 | V12 | TC-02: legacy memory loads with unknown fields intact |
| R-13 | Namespace traversal prevention | SC-03, AC-04 | V4 | TC-03: `../` in scope path rejected |
| R-24 | TTL-expired memories excluded from results | UC-13 | V11 | TC-04: expired memory not returned by recall/read/list |
| R-32 | Default classification applied when missing | AC-01 | V8 | TC-05: memory without classification gets default |
| R-33 | Classification filter in recall | UC-04 | V4 | TC-06: recall with filter excludes higher-sensitivity memories |
| R-34 | Classification-based access policies | SC-02 | V4 | TC-07: agent without clearance can't recall classified memory |
| R-35 | Classification downgrade audit logging | SC-05 | V7 | TC-08: downgrade event appears in audit log |
| R-43 | Schema changes don't invalidate existing memories | Migration | V14 | TC-09: adding required field with default doesn't break reads |
| R-51 | Frontmatter is valid YAML | UC-16 | V12 | TC-10: all metadata fields round-trip through YAML parse |
| R-53 | Custom fields in dedicated section | AC-03 | V5 | TC-11: custom field named "id" doesn't overwrite core field |
| R-54 | Backward-compatible deserialization | R-06 | V12 | TC-12: pre-framework memory deserializes with defaults applied |
| R-60 | Existing memories load without error | Migration | V12 | TC-13: full corpus of existing memories loads after upgrade |
| R-72 | Adversarial YAML deserialization | T-03 | V5 | TC-14: malformed YAML in file doesn't halt namespace scan |
| R-73 | Scope validation at ingest time | T-04, T-16 | V4 | TC-15: file with `../` in scope path rejected on read |
| R-74 | Commit SHA in audit log | T-05 | V7 | TC-16: write operation audit entry contains commit SHA |
| R-78 | Custom field YAML quoting | T-09 | V5 | TC-17: field value with YAML special chars round-trips safely |
| R-79 | Custom field name collision rejection | T-11 | V5 | TC-18: schema with custom field named "id" rejected at startup |
| R-80 | Field size limits | T-12 | V5 | TC-19: oversized string value rejected on write |
| R-81 | Downgrade audit durability | T-14 | V7 | TC-20: classification downgrade with failed audit log returns error |
