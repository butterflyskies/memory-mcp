# ADR-0021: Scope Affinity Defaults

## Status
Accepted

## Context
When `recall` or `list` are called without a scope filter, they return memories from all scopes — global and every project. This causes cross-project contamination: a memory stored for project A can leak into results when working in project B. Other agent memory systems (Claude Code auto-memory, Serena) isolate project memories by default.

## Decision
Change the default scope behavior for query operations (`recall`, `list`):
- Omitting scope returns global-only (was: everything)
- Passing `project:<name>` returns that project's memories **plus** global (compound filter)
- A new `"all"` sentinel explicitly opts into cross-project search
- Point operations (`remember`, `edit`, `read`, `forget`) are unchanged — scope targets a single memory

Tool descriptions guide agents to derive the project name from the basename of their working directory.

## Consequences
- Agents get project-isolated results by default, matching user expectations
- Cross-project search remains available via explicit `"all"` opt-in
- No migration needed — only query-time filtering changes, not storage
- Agents that previously relied on omitting scope to get everything must now pass `"all"`
