<!-- design-meta
status: approved
last-updated: 2026-05-25
phase: 5
-->

# Test Plan: Memory Metadata Framework

Derived from the SRTM in [requirements.md](requirements.md). Tests are organized by concern area, with each test traceable to one or more requirements.

## Unit Tests

### Schema Validation

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-01 | R-05 | `remember` with invalid custom field value (wrong type, e.g. string where number expected) returns validation error | Yes |
| TC-18 | R-79 | Schema config with custom field named `id` rejected at server startup with clear error message | Yes |
| TC-19 | R-80 | `remember` with string custom field exceeding max byte length (default 4096) rejected | Yes |
| TC-19b | R-80 | `remember` with list custom field exceeding max element count (default 100) rejected | Yes |
| TC-19c | R-80 | Schema config with nested object type in custom field definition rejected at startup | Yes |

### Scope / Namespace

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-03 | R-13 | `remember` with scope containing `../` rejected | Yes |
| TC-03b | R-13 | `remember` with absolute path scope (`/etc/foo`) rejected | Yes |
| TC-03c | R-13 | `remember` with scope containing null bytes rejected | Yes |
| TC-21 | R-10 | `remember` with hierarchical scope `org/team/project` stores to correct directory path | Yes |
| TC-22 | R-11 | Legacy scope `"global"` parses to `Scope::Root` | Yes |
| TC-22b | R-11 | Legacy scope `"project:foo"` parses to `Scope::Path("foo")` | Yes |
| TC-23 | R-12 | `recall` with subtree scope `engineering` matches memories in `engineering/ml` and `engineering/infra` | Yes |
| TC-37 | R-11 | Legacy behavior: `recall` with `project:foo` scope still includes global memories (behavioral compat, not just parsing) | Yes |

### Retention

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-04 | R-24 | TTL-expired memory excluded from `recall` results | Yes |
| TC-04b | R-24 | TTL-expired memory excluded from `read` (returns not-found, not expired status) | Yes |
| TC-04c | R-24 | TTL-expired memory excluded from `list` results | Yes |
| TC-24 | R-20 | Memory with `retention: { type: ttl, duration: 7d }` computes correct `expires_at` from `created_at` | Yes |
| TC-25 | R-21 | Memory with `retention: { type: condition, expr: "PR merged" }` returns condition in metadata on read | Yes |
| TC-26 | R-22 | Memory with `retention: { type: evergreen }` never excluded by TTL filter | Yes |
| TC-27 | R-23 | Memory without retention field gets deployment default applied at read time | Yes |
| TC-28 | R-71 | Memory with frontmatter retention more permissive than deployment policy: deployment policy takes precedence | Yes |

### Classification

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-05 | R-32 | Memory stored without classification gets deployment default label | Yes |
| TC-06 | R-33 | `recall` with classification filter excludes memories with higher-sensitivity labels | Yes |
| TC-29 | R-31 | `remember` with classification label not in deployment config rejected | Yes |
| TC-30 | R-30 | Memory with multiple classification labels stores and retrieves correctly | Yes |

### Serialization

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-10 | R-51 | All metadata fields (core, standard, custom) round-trip through YAML serialize/deserialize | Yes |
| TC-11 | R-53 | Custom field values always appear under `custom:` section, never at top level | Yes |
| TC-17 | R-78 | Custom field value containing `\n`, `:`, `---`, quotes serializes as double-quoted scalar and round-trips correctly | Yes |
| TC-17b | R-78 | Custom field value containing YAML document separator `---` does not terminate frontmatter | Yes |
| TC-31 | R-52 | `MemorySerializer` trait can be implemented independently (compile-time check: trait is object-safe or has clear bounds) | Yes |

## Integration Tests

### Migration / Backward Compatibility

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-02 | R-06 | Memory file with unknown frontmatter fields loads; unknown fields preserved in round-trip | Yes |
| TC-09 | R-43 | Adding a required custom field with a default to the schema: existing memories without the field load with default applied | Yes |
| TC-12 | R-54 | Pre-framework memory (no retention, no classification, no custom fields) deserializes with deployment defaults | Yes |
| TC-13 | R-60 | Load the full existing memory corpus after upgrading — zero errors, all memories accessible | Yes |
| TC-32 | R-61 | Read a pre-framework memory: response includes default retention and classification from deployment config | Yes |

### Ingest Validation (External Tool Boundary)

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-14 | R-72 | Write a malformed YAML file directly to repo, then `list` the namespace: malformed file skipped, other memories returned, structured log emitted | Yes |
| TC-15 | R-73 | Write a file with `../` in scope path directly to repo, then read: path rejected at ingest, logged | Yes |
| TC-33 | R-76 | Write a file exceeding max size directly to repo, then `list`: oversized file skipped with log | Yes |
| TC-34 | R-72 | Write a file with truncated YAML (missing closing `---`): deserializer handles gracefully | Yes |

### Audit Logging

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-08 | R-35, R-81 | `edit` that downgrades classification: audit log entry written before git commit | Yes |
| TC-16 | R-74 | `remember` operation: audit log entry contains git commit SHA | Yes |
| TC-20 | R-81 | `edit` classification downgrade with simulated audit log failure: operation returns error, git repo unchanged | Yes |
| TC-35 | R-83 | `remember` with source field: audit log entry contains the source value | Yes |

### Access Policy (stub / future)

| Test ID | Requirement | Description | Automated? |
|---------|-------------|-------------|------------|
| TC-07 | R-34 | With classification access policy configured: agent without clearance gets empty recall results for classified memories | Yes |
| TC-36 | R-82 | (Future) Namespace probe: query for non-existent vs access-denied namespace returns identical responses | No (manual, deferred to policy engine) |

## Test Fixtures

### Required fixtures

- **Pre-framework memory corpus**: a set of memory files in the old format (no retention, no classification, no custom fields, `scope: Global` and `scope: Project("foo")` format)
- **Deployment config variants**: minimal (no custom fields), standard (retention + classification), full (custom fields with various types)
- **Adversarial YAML files**: malformed frontmatter, oversized files, traversal paths, injection payloads
- **Time-sensitive memories**: memories with TTL near expiry for retention filter tests (use test clock / mockable time)

### Test environment

- All tests use an in-memory git repo (existing test infrastructure)
- Audit log tests use an in-memory SQLite database
- Time-dependent tests use a mockable clock (inject `now()` function)
- Schema validation tests load config from test-specific TOML files

## Coverage Matrix

| Concern | Requirements | Test Cases | Coverage |
|---------|-------------|------------|----------|
| Schema validation | R-05, R-79, R-80 | TC-01, TC-18, TC-19, TC-19b, TC-19c | Full |
| Namespace/scope | R-10, R-11, R-12, R-13 | TC-03, TC-03b, TC-03c, TC-21, TC-22, TC-22b, TC-23 | Full |
| Retention | R-20-R-26, R-71 | TC-04, TC-04b, TC-04c, TC-24-TC-28 | Full |
| Classification | R-30-R-34 | TC-05, TC-06, TC-29, TC-30 | Full |
| Serialization | R-50-R-54, R-78 | TC-10, TC-11, TC-17, TC-17b, TC-31 | Full |
| Migration | R-06, R-43, R-54, R-60, R-61 | TC-02, TC-09, TC-12, TC-13, TC-32 | Full |
| Ingest validation | R-72, R-73, R-76 | TC-14, TC-15, TC-33, TC-34 | Full |
| Audit logging | R-35, R-74, R-81, R-83 | TC-08, TC-16, TC-20, TC-35 | Full |
| Access policy | R-34, R-82 | TC-07, TC-36 | Partial (TC-36 deferred) |
