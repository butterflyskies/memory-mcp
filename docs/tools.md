# MCP tool reference

memory-mcp exposes twelve tools. Tool schemas returned during MCP discovery are
the authoritative machine-readable contract; this page explains how the tools
fit together.

## Memory lifecycle

| Tool | Purpose |
|---|---|
| `remember` | Store content, tags, source, and an optional scope; commit it to git and index it. |
| `read` | Fetch one memory's complete content and metadata by name and scope. |
| `edit` | Replace content or tags while preserving omitted fields. |
| `move` | Atomically move a memory to another scope, optionally renaming it. |
| `forget` | Delete a memory from git and the search indexes. |
| `list` | List memory summaries without full content. |

Memory names may contain up to three path components. Names and scopes are
validated before they become filesystem paths.

## Retrieval

### `recall`

`recall` accepts a natural-language `query`, an optional `scope`, and an
optional `limit` (default 5).

It runs two independent retrieval strategies:

1. local embeddings search the scope-partitioned HNSW vector indexes;
2. Tantivy searches the in-memory BM25 lexical index, with exact phrases ranked
   ahead of term-only matches.

The ranked lists are merged with reciprocal rank fusion. A result includes:

- `name`, `scope`, and `tags`;
- a content snippet of at most 500 characters;
- `truncated` and `content_length`, so a caller knows when to use `read`;
- `match_type`: `semantic`, `lexical`, or `both`;
- `distance`: cosine distance for semantic hits, or `-1.0` for lexical-only
  hits;
- a batch-level `recall_id` used by the feedback tools.

Lower non-negative distances are more similar. Do not interpret `-1.0` as a
high-confidence semantic match; it means the result had no embedding distance.

## Scopes

Scopes are hierarchical namespace paths:

- omit scope or pass `global` to query global memories only;
- pass `my-project` to query that subtree plus global memories;
- pass `org/team` to include `org/team` and descendant scopes plus global;
- pass `all` to explicitly query every scope.

Point tools (`remember`, `read`, `edit`, `move`, and `forget`) address one exact
scope. Omitting their scope targets global.

Scopes organize storage and retrieval. They do not enforce authorization.

## Synchronization

`sync` pulls before pushing by default. It requires a configured remote for
remote work; local-only deployments return without trying to push. Git conflicts
are resolved using the timestamps stored in memory frontmatter, with warnings
recording the resolution.

## Recall feedback

Every recall returns a `recall_id`. Once an agent decides whether a result was
useful, it can report:

- `mark_applied` for one result;
- `batch_mark_applied` for several results in one transaction;
- `recall_stats` to inspect applied, maybe, not-applied, and unknown results by
  distance bucket.

Verdicts are `applied`, `maybe`, or `not_applied`. Confidence is `high`,
`medium`, or `low`. The feedback log is local SQLite telemetry; it is not stored
in the Markdown memory repository or synced through git.

## Timing metadata

Successful MCP tool results include
`_meta["memory-mcp/serverProcessingDurationMs"]`. It measures work from the
server's tool-handler boundary through result conversion. It does not include
network or client processing time.
