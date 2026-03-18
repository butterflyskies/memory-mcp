# ADR-0015: Multi-user memory namespacing (planned)

## Status
Proposed

## Context
memory-mcp should support multiple users, each with isolated memories. Future work may
include group-scoped memories (shared organizational context) with access control for
membership and write permissions.

## Decision
Round 1 deploys as single-user. Round 2 adds per-user memory isolation via OIDC identity
(Dex). The storage model for multi-user (repo-per-user, subdirectories, or branches) is
deferred to Round 2 planning. Group-scoped memories are a future design discussion — the
architecture should not preclude them but need not implement them initially.

## Consequences
- Round 1 is simpler: one git repo, one vector index, no auth middleware
- Round 2 must decide storage isolation model before implementing
- Domain `memories.svc.echoes` will eventually serve both MCP API and a CRUD web UI
- Group-scoped memories introduce authorization complexity (who writes, who manages membership)
