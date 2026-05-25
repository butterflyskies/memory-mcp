# ADR-0032: User-defined classification labels with policy-driven enforcement

## Status
Accepted

## Context
Memories have no sensitivity level. A private DM summary and a public architecture note look identical in the store. Privacy enforcement depends on the agent remembering provenance, which fails across sessions. Different deployments need different classification taxonomies (enterprise: public/internal/confidential/restricted; personal: public/group/private).

## Decision
Each memory has exactly one classification label (single-valued, not multi-label). Multi-label classification was rejected because it blurs sensitivity levels into taxonomy — one effective classification wins. The deployment config defines the valid label set with explicit rank ordering and a default. The server:
- Validates the label against the config on write (rejects unknown labels)
- Applies the default when no classification is specified
- Returns classification in all read/recall/list responses
- Supports optional classification-based filtering in recall (comparing rank)

Classification has no hardcoded semantics — the server doesn't know what "confidential" means, only that it has rank 30. Policy enforcement (who can recall what) is a configurable layer that maps classification + namespace + identity to allow/deny decisions. Without the auth framework (#115), classification is retrieval control and operator guidance, not security isolation.

Classification downgrades (e.g. confidential → public) are treated as sensitive operations: audit log entry must be durably written before the git commit.

## Consequences
- No hardcoded tiers — deployments define their own taxonomy
- Classification is metadata, not access control (until #115 ships the policy engine)
- Labels provide no filesystem-level protection — repo ACLs are a separate concern
- Downgrade audit is a pre-write gate, not a post-write side effect
