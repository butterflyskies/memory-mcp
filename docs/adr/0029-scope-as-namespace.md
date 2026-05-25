# ADR-0029: Scope as pure namespace, access as policy

## Status
Accepted

## Context
The current `Scope` enum (`Global`, `Project(String)`) conflates two concerns: where a memory is organized (namespace) and who can access it. `Project("foo")` means both "lives in foo" and "only visible when querying foo." This can't express hierarchies, and access control is implicit in the namespace rather than explicit in policy.

Users already work around this with naming conventions (e.g. `project:person-<name>` as a pseudo-namespace).

## Decision
Replace the scope enum with a path-based namespace model:
- `Scope::Root` (was `Global`) — the `/` namespace
- `Scope::Path(String)` — hierarchical, e.g. `org/team/project`

Backward compatibility: `"global"` parses to `Root`, `"project:foo"` parses to `Path("foo")`.

Scope is purely organizational — it determines where the memory lives (directory structure in git, namespace in queries). Access control is a separate policy-layer concern that references scope, classification, and identity. `ScopeFilter` gains subtree matching: querying `engineering` matches `engineering/ml` and `engineering/infra`.

## Consequences
- Scope paths validated against traversal attacks (`..`, absolute paths) at both write and read time
- On-disk directory structure follows namespace path
- Access control deferred to policy engine (#115) rather than being baked into scope
- Subtree queries enable hierarchical organization without breaking flat usage
