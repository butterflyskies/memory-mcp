<!-- design-meta
status: approved
last-updated: 2026-05-25
phase: 4
-->

# Threat Model: Memory Metadata Framework (Lightweight)

Focused STRIDE analysis on two key trust boundaries. Full systematic enumeration deferred — this is metadata infrastructure, not authentication.

---

## Scope

This document analyzes two trust boundary crossings introduced by the metadata framework redesign. Out of scope: transport security (stdio/SSE), authentication and identity (#115), supply chain, and the vector index subsystem.

**Actors:**

| Actor | Trust level |
|-------|-------------|
| AI Agent | Untrusted — input is arbitrary |
| MCP Server process | Trusted — controlled execution environment |
| Git repo | Trusted at rest, but externally writable |
| External tools (Obsidian, scripts, CI) | Unmediated — bypass server entirely |
| Admin | Trusted — controls deployment config |

---

## Boundary 1: External Tools → Git Repo (unmediated)

**Data flow:** External tools read and write markdown files directly to the git repo. No schema validation, no policy enforcement. The server encounters these files at recall/read time and must handle them gracefully.

**Attack surface:** Filesystem + git. Any process with repo access can write arbitrary YAML frontmatter.

### STRIDE Analysis

| # | Category | Threat | Likelihood | Impact | Mitigation |
|---|----------|--------|------------|--------|------------|
| T-01 | **Tampering** | An attacker with repo access could modify a memory file's `classification` frontmatter field to downgrade sensitivity (e.g. `confidential → public`), causing the server to serve it to agents with lower clearance at next read. The server has no integrity check against the last-known classification. | Medium | High | Compute and persist a classification hash/signature on server-write; verify on server-read. Flag mismatch as a tamper event. See NEW-REQ-01. |
| T-02 | **Tampering** | A malicious external write could replace `retention.variant: Ttl` (expires in 7d) with `retention.variant: Evergreen`, causing a memory that should have been deleted to persist indefinitely and pollute future recalls. | Medium | Medium | Server-side retention cannot be enforced for externally-written files. Mitigate by: (a) logging a warning when server reads a retention upgrade (TTL → Evergreen) on a file it previously committed with a stricter policy; (b) making the reaper re-validate retention against deployment defaults rather than trusting frontmatter values blindly. See NEW-REQ-02. |
| T-03 | **Tampering** | An attacker could inject malformed or adversarial YAML into a memory file's body or frontmatter section, causing the server's YAML parser to crash, enter an error state, or misparse adjacent fields on ingest. (This is particularly relevant for `custom:` subsections with nested structures.) | Medium | Medium | The deserializer must treat externally-written files as untrusted: wrap YAML parse in a recoverable error boundary, never panic, log and skip malformed files rather than halting. Existing R-54 requires backward-compatible deserialization but doesn't address adversarial input. See NEW-REQ-03. |
| T-04 | **Tampering** | A script or CI pipeline could write a memory file with a `scope_path` containing `../` or an absolute path component, which the server then resolves during a subtree query, potentially returning files outside the intended namespace or resolving to paths outside the repo. | Low | High | R-13 requires traversal prevention on agent-submitted scope values, but the same check must run at read/ingest time for externally-written files. Server must canonicalize and validate `scope_path` on every read, not only on write. See NEW-REQ-04. |
| T-05 | **Repudiation** | External tools write directly to git without going through the MCP server, so no audit log entry is created. A classification downgrade or retention extension committed by a script is invisible to the server's audit trail. | High | Medium | Server cannot prevent this — git log is the only record. Mitigate by: (a) ensuring the audit log records the git `commit_sha` for every server-written memory (enabling diff-based detection of out-of-band changes); (b) documenting the gap explicitly. See NEW-REQ-05. |
| T-06 | **Information Disclosure** | A misconfigured CI pipeline with read access to the repo could exfiltrate all memories regardless of their `classification` label, because classification is advisory metadata — the files are plaintext in the repo and there is no encryption at rest. | High | High | This is an architectural limitation acknowledged in requirements (V6: stored cryptography deferred). Mitigate by documenting clearly that classification labels are access-control hints enforced only through the MCP server path, not filesystem ACLs. Encryption at rest is a future requirement. See NEW-REQ-06 (documentation requirement). |
| T-07 | **Denial of Service** | An external tool could write a very large number of files into the repo, or write extremely large files, causing the server's recall/list operations to time out or exhaust memory when scanning the namespace. | Low | Medium | Implement configurable limits: max file size on ingest (skip and log oversized files), max namespace depth traversal count. These complement but don't replace filesystem-level access controls. See NEW-REQ-07. |
| T-08 | **Elevation of Privilege** | An external tool could write a memory into a namespace it shouldn't have access to (e.g. writing into `org/team-b/` from a CI pipeline scoped to `org/team-a/`). The server would then serve this memory to `team-b` agents on recall. | Medium | High | Namespace access policies (R-11, R-14) are enforced on the agent→server path, not at the git layer. This boundary has no policy enforcement. Mitigation requires either: (a) git-layer controls (branch protection, path-restricted tokens), or (b) the server treating any file not committed by its own service account as requiring re-validation against the namespace policy for the writing context. See NEW-REQ-08. |

---

## Boundary 2: Agent → Server (custom metadata fields)

**Data flow:** The agent passes custom metadata field values as part of `remember` / `edit` tool calls. The server validates these against the deployment schema, then the serialization layer writes them into YAML frontmatter. Other tools (including Obsidian) later parse that frontmatter.

**Attack surface:** The MCP tool call arguments. The agent is untrusted; its inputs may come from tool outputs, LLM generation, or user-provided content that has passed through an agent.

### STRIDE Analysis

| # | Category | Threat | Likelihood | Impact | Mitigation |
|---|----------|--------|------------|--------|------------|
| T-09 | **Tampering / YAML injection** | An agent passes a custom field value containing YAML special characters — newlines, colons, quote sequences, or block scalar markers — that break out of the scalar context in the rendered frontmatter. For example, a `description` field value of `"foo\nclassification: public"` could inject a second `classification` key into the frontmatter if the serializer doesn't properly quote the value. A downstream YAML parser (Obsidian, scripts) might then read the injected key instead of the legitimate one. | Medium | High | The serializer must always emit custom field scalar values as quoted strings (single or double), never bare scalars. Values containing characters that cannot be safely single-quoted must be double-quoted with proper escaping. This is a serializer implementation requirement, not just a schema validation requirement. AC-03 and R-53 address the namespace isolation (custom fields in a dedicated section) but don't specify quoting strategy. See NEW-REQ-09. |
| T-10 | **Tampering / YAML injection** | An agent passes a custom field value that is a multi-line string containing `---` (YAML document separator). If the serializer emits it as a block scalar without careful handling, a downstream parser could interpret the separator as ending the frontmatter block prematurely, causing subsequent content to be treated as the memory body rather than frontmatter. | Low | Medium | Block scalars in frontmatter values must be forbidden or carefully escaped. The serializer should normalize multi-line values: either reject them (validation rule for string-typed fields) or fold them into a single line. See NEW-REQ-09 (same serializer hardening requirement). |
| T-11 | **Tampering** | An agent passes a custom field named `id`, `created_at`, `updated_at`, or another core field name, attempting to overwrite a server-managed field via the custom namespace. If the serializer merges custom fields into the top-level frontmatter without a collision check, the injected value wins. | Medium | High | R-53 requires custom fields to appear in a dedicated `custom:` YAML subsection specifically to prevent this. This requirement must be enforced both at the schema definition level (custom field names must not collide with core/standard fields) and at serialization time (custom fields are always nested under `custom:`, never promoted). Confirm this is explicitly checked at schema load time, not assumed. See NEW-REQ-10. |
| T-12 | **Denial of Service** | An agent passes an extremely large string value for a custom field (e.g. a base64-encoded blob as a "description"), causing the serialized frontmatter to grow to megabytes. This bloats the git commit, slows subsequent reads, and may exhaust memory in the YAML parser for any tool that opens the file. | Low | Medium | Schema validation must enforce a maximum byte length on string-typed custom field values. The limit should be configurable per field in `CustomFieldDef` with a safe default (e.g. 4 KB). See NEW-REQ-11. |
| T-13 | **Denial of Service** | An agent passes a list-typed custom field with a very large number of elements, or a deeply nested structure that the schema's type system notionally supports but the serializer doesn't defend against. | Low | Low | Schema validation must enforce max-length on list-typed fields. Nested objects are not supported in `CustomFieldDef` (field types are flat: `string`, `number`, `bool`, `string[]`) — this should be enforced at schema load time to prevent future type creep from opening this vector. See NEW-REQ-11. |
| T-14 | **Repudiation** | An agent downgrades a memory's classification from `confidential` to `public` via an `edit` call. R-35 says this should be logged, but if the audit log write fails (SQLite error, disk full), the downgrade is committed to git without any audit trail. | Low | High | Audit log writes for classification changes must be durably committed before the git write completes, or the operation must fail atomically. "Should" in R-35 should be promoted to "Must" for classification downgrades specifically. See NEW-REQ-12. |
| T-15 | **Information Disclosure** | An agent crafts a `recall` query that uses scope path patterns to probe whether memories exist in namespaces it doesn't have access to (e.g. by observing whether a wildcard subtree query returns results vs. an empty set). Even if memory content is withheld, the presence/absence of a namespace can leak organizational structure. | Low | Low | Namespace probing via empty-result oracle is a low-severity information leak in most deployments. If namespace privacy is required, the policy engine (PE, stub in architecture) should return identical empty responses for both "no access" and "no results". This is a future policy engine concern; document the gap now. See NEW-REQ-13. |
| T-16 | **Elevation of Privilege** | An agent supplies a `scope` value in `remember` or `edit` containing a `..` component or an absolute path (e.g. `/etc/`), attempting to write a file outside the repo's working directory. R-13 requires traversal prevention but this must also cover the serialization→git write path: the `scope_path → directory_path` mapping must canonicalize against the repo root and reject any path that escapes it. | Low | High | This is a defense-in-depth requirement: R-13 validates the scope value, and the git repository adapter must independently verify the resolved path is under the repo root before writing. Two independent checks, both must pass. This is already partially addressed by R-13 but deserves an explicit implementation requirement at the adapter layer. See NEW-REQ-04 (same requirement, both boundaries). |
| T-17 | **Spoofing** | In a multi-agent deployment, Agent A cannot prove to the server that it is the same agent that wrote a memory. There is no per-agent identity at the MCP boundary (#115 deferred). An agent could read and re-write another agent's memory with a lower classification or different retention, impersonating the original writer. | Medium | High | This is a known gap pending #115 (auth framework). In the interim: the server should record the `source` field (agent-provided, not validated) in the audit log for all write operations. This provides weak provenance evidence. Document that without #115, classification and retention integrity rely on trust in the MCP client process, not cryptographic identity. See NEW-REQ-14. |

---

## Special Cases

### Classification downgrade attacks

Covered by T-01 (external write) and T-14 (agent edit). The shared mitigation: classification downgrades are a special write operation requiring (a) explicit operator acknowledgement in multi-agent deployments, (b) durable audit log entry before storage commit, and (c) ideally a tamper-evident record of the pre-downgrade label (commit hash is sufficient for git-backed storage).

### Namespace traversal

Covered by T-04 (external ingest path) and T-16 (agent write path). R-13 addresses the agent write path; NEW-REQ-04 extends this to the ingest path. Both the scope path validator and the git adapter must independently enforce the constraint — defense in depth is appropriate here because the consequences (file write outside repo) are severe.

### YAML injection

Covered by T-09 and T-10. The root cause is insufficient serializer hardening: schema validation alone is not sufficient because a valid string value can still contain YAML structural characters. The serializer must be the final line of defense and must always emit custom field values in a form that is structurally safe regardless of content.

### Retention policy manipulation

Covered by T-02 (external upgrade) and AC-02 from requirements (agent evergreen abuse). The server should not trust frontmatter retention values for externally-written files without comparison against deployment policy. An evergreen retention on a memory whose namespace has a 90-day policy maximum should trigger a warning or be overridden.

---

## New Requirements

| ID | Requirement | Addresses | ASVS Category |
|----|-------------|-----------|---------------|
| NEW-REQ-01 | The server shall record the expected classification label in the git commit metadata (or a sidecar index) and verify it matches the frontmatter on read. Mismatch shall be logged as a tamper event and the memory shall be returned with a `tamper_suspected: true` flag. | T-01 | V12: Files and Resources |
| NEW-REQ-02 | The retention reaper and recall filter shall evaluate retention policy against the deployment config defaults, not frontmatter alone. If a memory's frontmatter retention is more permissive than the deployment policy for its classification/namespace, the deployment policy takes precedence. | T-02 | V11: Business Logic |
| NEW-REQ-03 | The memory deserializer shall treat all externally-written files as adversarial input: YAML parse errors must be caught and logged at the file level, must not propagate exceptions to the caller, and must not halt namespace traversal. Malformed files are skipped with a structured log entry. | T-03 | V5: Validation, Sanitization, Encoding |
| NEW-REQ-04 | Scope path validation (canonicalization, `..` rejection, absolute path rejection) shall run at both (a) agent write time (schema validator) and (b) server read/ingest time (git adapter). These are independent checks. | T-04, T-16 | V4: Access Control |
| NEW-REQ-05 | The server shall record the git commit SHA in all audit log entries for write operations. This enables detection of out-of-band changes by diffing the recorded SHA against the current commit for a file. | T-05 | V7: Error Handling and Logging |
| NEW-REQ-06 | Documentation shall state explicitly that `classification` labels are enforced only through the MCP server path and provide no filesystem-level access control. Deployments storing sensitive memories must apply appropriate filesystem/repository ACLs independently. | T-06 | V8: Data Protection |
| NEW-REQ-07 | The server shall apply configurable limits at ingest: max file size (default: 1 MB, skip and log files exceeding this) and max namespace depth (default: 10 path components, reject deeper paths). | T-07 | V12: Files and Resources |
| NEW-REQ-08 | Documentation shall state that the external tool boundary has no policy enforcement and that namespace isolation for external tools requires git-layer access controls (e.g. path-restricted deploy tokens, repo ACLs). | T-08 | V4: Access Control |
| NEW-REQ-09 | The YAML frontmatter serializer shall always emit custom field string values as quoted YAML scalars (double-quoted with proper escape sequences). Block scalars, bare scalars, and multi-line strings for custom field values are forbidden. The serializer must not rely on upstream validation to guarantee structural safety. | T-09, T-10 | V5: Validation, Sanitization, Encoding |
| NEW-REQ-10 | The schema loader shall reject any `CustomFieldDef` whose name collides with a core field name (`id`, `name`, `scope`, `created_at`, `updated_at`) or a reserved standard field name (`tags`, `source`, `retention`, `classification`). This check runs at server startup, not at write time. | T-11 | V5: Validation, Sanitization, Encoding |
| NEW-REQ-11 | Schema validation shall enforce a maximum byte length on string-typed custom field values (configurable, default: 4096 bytes) and a maximum element count on list-typed fields (configurable, default: 100). Nested object types shall not be permitted in `CustomFieldDef` — the type system is intentionally flat. | T-12, T-13 | V5: Validation, Sanitization, Encoding |
| NEW-REQ-12 | Classification downgrade audit log writes shall be durable before the corresponding git write is committed. If the audit log write fails, the `edit` operation shall fail and return an error to the agent rather than committing the downgrade silently. This upgrades R-35 from "Should" to "Must" for downgrade operations specifically. | T-14 | V7: Error Handling and Logging |
| NEW-REQ-13 | (Future / policy engine stub) When namespace access policy is configured, the server shall return identical empty-result responses for "access denied" and "no memories found" to prevent namespace probing via oracle. This applies to `recall`, `list`, and `read` operations. | T-15 | V4: Access Control |
| NEW-REQ-14 | Pending #115 (auth framework), the server shall record the agent-supplied `source` field (or a configurable agent identity hint) in all audit log entries for write operations. This provides weak provenance evidence in multi-agent deployments and is explicitly documented as non-cryptographic. | T-17 | V7: Error Handling and Logging |

---

## Architectural Changes

Two changes go beyond requirements and affect component boundaries:

**1. Git adapter path canonicalization (defense in depth)**

The git repository adapter (`repo.rs`) must independently validate that the resolved file path for any write or read operation falls within the repo root. This is separate from and in addition to the scope path validation in the schema validator. The rationale: the scope path validator operates on the logical namespace value; the git adapter operates on the physical filesystem path. A bug in the scope→path mapping (e.g. unexpected percent-encoding, Unicode normalization) could bypass scope validation but would be caught by a root-confinement check at the adapter.

**2. Audit log as a pre-write gate for classification downgrades**

The current architecture (sequence diagram) places audit logging as a post-write side effect. For classification downgrades specifically, the write order must change: audit log entry committed → git write committed. This requires the `remember`/`edit` tool handler to treat the audit log as a prerequisite, not a follower, for this class of operation. The architecture diagram's `TH → RL` edge should be annotated as "pre-write gate for classification changes."

---

## Implementation Detail: In-Memory Git Store

The current implementation maintains the git repo in memory and overwrites the on-disk state on every mutation. This has significant implications for the external tools threat boundary:

**While the server is running:** External writes to the on-disk repo are ephemeral — the next server mutation clobbers them from in-memory state. The external edit is silently lost. This **accidentally mitigates** T-01, T-02, T-04, and T-08 for the running-server case.

**While the server is stopped:** External writes to the on-disk repo are durable — they are read into memory on the next server startup. This is the real attack window.

**Implications for threat severity:**
- T-01 through T-08 severity should be read as "during server downtime only" for write-based threats
- Startup/ingest validation (NEW-REQ-03, NEW-REQ-04) becomes the critical defense point
- The tamper detection mechanism (NEW-REQ-01) is most valuable at startup, comparing on-disk state against the last known good commit SHA
- External tools like Obsidian have a read-only view while the server runs; their writes only persist if made during downtime

This does NOT affect read-based threats (T-06: information disclosure via repo access) — those apply regardless of server state.

---

## Assumptions and Deferred Items

| Item | Status |
|------|--------|
| Agent authentication and identity | Deferred to #115. Without it, all agent-provided identity claims (source, author) are advisory only. |
| Encryption at rest for classified memories | Deferred. Classification labels are ACL hints, not encryption keys. |
| Transport security (stdio, SSE) | Out of scope for this document. |
| Vector index tamper resistance | Out of scope. The vector index is a cache; ground truth is the git repo. |
| Supply chain (YAML parser vulnerabilities) | Out of scope. Pin dependency versions; monitor advisories. |
