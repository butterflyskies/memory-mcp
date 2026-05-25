# ADR-0031: Write-time retention metadata with split enforcement

## Status
Accepted

## Context
Memories accumulate forever. A session handoff and a persona definition are stored identically — both persist until manually deleted. The context for making retention decisions is richest at creation time and degrades across sessions and compactions. There is no mechanism to express at write time that a memory should expire.

## Decision
Retention is a standard metadata field set at write time with three variants:
- **TTL** (`{ type: ttl, duration: 7d }`) — server computes `expires_at` from `created_at + duration` and mechanically excludes expired memories from results
- **Condition** (`{ type: condition, expr: "PR #247 merged" }`) — server stores the condition as opaque metadata; the agent evaluates it at read time and calls `forget` when met
- **Evergreen** (`{ type: evergreen }`) — explicit marker that the memory should never auto-expire

Memories without retention metadata inherit the deployment's default (configured in TOML). The retention reaper evaluates against deployment config defaults, not frontmatter alone — if frontmatter is more permissive than policy, policy wins.

Cleanup is lazy: TTL-expired memories are filtered at query time and eventually deleted by a background or on-access reaper. No big-bang sweep.

## Consequences
- Server handles TTL mechanically; agents handle conditions — clean split of responsibility
- Deployment policy can override per-memory retention (prevents evergreen abuse)
- External tools that edit retention frontmatter during server downtime are subject to policy override on next startup
- Condition expressions are opaque strings — no server-side evaluation, no expression language to maintain
