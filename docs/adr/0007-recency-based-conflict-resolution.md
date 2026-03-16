# ADR-0007: Recency-based conflict resolution

## Status
Accepted

## Context
When pulling from a remote, memory files may conflict if the same memory was edited
on two devices. We need a deterministic, automated resolution strategy since sync is
often triggered by AI agents without human intervention.

## Decision
Compare `updated_at` timestamps from YAML frontmatter — most recent wins. If timestamps
are equal or unparseable, fall back to "ours-wins" (keep local version). Conflicts and
their resolution are logged as warnings for auditability.

## Consequences
- No manual conflict resolution required — sync is fully automated.
- A memory edited more recently on the remote will overwrite a stale local copy (correct
  in most workflows where the user moves between devices sequentially).
- If two devices edit the same memory within the same second, local wins arbitrarily.
- Conflict history is preserved in git — `git log` shows the merge commit with both parents.
