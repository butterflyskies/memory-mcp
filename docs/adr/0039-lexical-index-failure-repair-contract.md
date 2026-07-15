# ADR-0039: Lexical index failure/repair contract

## Status
Accepted

## Context
ADR-0038 made lexical writes "best-effort: a failure logs a warning and
degrades that memory to semantic-only until the next startup rebuild."
Characterization with failure-injection tests (#310) showed the real
behavior was worse than the stated best-effort story, because Tantivy
writer ops queue until `commit()` and every failure path returned early
**without rollback**:

- **Ghost commits.** Ops queued before a mid-batch failure were silently
  committed by the *next* successful batch.
- **Commit-failure replay.** A batch whose commit failed was committed
  wholesale by the next batch.
- **Rebuild wipe.** A failed rebuild stranded `delete_all_documents`; the
  next unrelated upsert's commit wiped the entire index, and search kept
  returning `Ok` (empty) with no signal.
- **Silent staleness.** After any failed mirror of a git write, search
  kept serving pre-failure content as healthy results. Nothing detected
  the divergence; the only heal was a process restart.
- **Cancellation strand.** `apply_async` dispatched its batch lazily (on
  first poll), so a caller dropping the future — request cancellation —
  after the git write but before the poll silently lost the mirror.

The method for this change was characterize-first: the failure-injection
tests were written before any fix, run against the untouched #308 code to
produce the evidence above, then kept as the permanent acceptance proof.

## Decision
Define and implement a failure/repair contract for the lexical index:

1. **Git remains authoritative.** The lexical index is derived state,
   never trusted over the repo.
2. **No silent stale lexical results.** Any failed or interrupted index
   mutation leaves the index either provably consistent with git truth or
   flagged **degraded** — never silently divergent. Every failure path in
   `apply`/`rebuild_from` rolls back the batch's uncommitted ops (so a
   later commit can never ship them) and marks the index degraded.
3. **Degraded state is observable.** Degradation logs a `tracing` warn
   with the reason and is queryable via `LexicalIndex::is_degraded` /
   `LexicalIndex::is_repairing`. While degraded, `search` errors instead
   of serving potentially stale results; hybrid recall already treats a
   lexical error as "semantic-only for this query".
4. **Deterministic repair.** Repair is a rebuild from git truth through
   the same code path as the startup rebuild
   (`search::rebuild_lexical_from_repo`), on the blocking pool,
   single-flight (`try_claim_repair`). It is triggered in the background
   wherever degradation is observed: at each mutation-mirror failure site
   and from the recall handler (`search::spawn_lexical_repair`). Recall
   serves semantic-only for the whole degraded window, including during
   the rebuild.
5. **Cancellation is a non-event.** `apply_async` dispatches its batch to
   the blocking pool **eagerly** (before the returned future is first
   polled), so dropping the future never strands a half-mirrored git
   write: the batch still runs to completion, either converging with git
   truth or failing and flagging degraded. Handlers mirror after the repo
   save with no intervening await point, so request cancellation cannot
   separate a git write from its mirror dispatch.

Detection mechanism — epoch bookkeeping, the smallest thing that makes
the clauses true:

- `dirty_epoch` counts divergence events (failed/interrupted mutations);
  `clean_epoch` records the `dirty_epoch` value captured by the last
  successful rebuild **before its repo listing** (`RebuildToken`).
  Degraded ⇔ `clean_epoch < dirty_epoch`. Divergence events after the
  token capture therefore survive the rebuild and force another pass.
- `applied_batches` counts committed mirrors. A rebuild whose token
  predates a concurrently committed mirror re-flags the index instead of
  silently dropping that mirror (the repo listing may not include it);
  repeated repair converges once a rebuild window is quiescent.
- Write ordering in handlers: `remember`/`edit` now mirror **after** the
  repo save (previously before), so a failed save can no longer leave the
  index serving content the repo never accepted.

A freshly constructed (empty) index is *not* degraded: unbuilt-at-boot is
a lifecycle state, not a divergence event. Lifecycle states
(Booting / SemanticReady / Rebuilding / FullyReady / Degraded / Failed)
and their exposure through health endpoints and probes are specified in
the startup/lifecycle design doc (Lain's lane); this contract defines
what the failure-side states mean and their transitions:
`is_degraded && is_repairing` ⇒ Rebuilding, `is_degraded && !is_repairing`
⇒ Degraded, construction failure (disabled index, every op errors) ⇒
Failed. The readiness surface never gates on lexical state.

## Consequences
- Failure-injection tests across all mutation paths (remember, edit,
  forget, move, incremental reindex, startup rebuild) plus cancellation
  are the permanent acceptance proof: no silent divergence, deterministic
  post-repair convergence, semantic-only recall while degraded.
- A degraded index refuses lexical queries until repaired; recall quality
  temporarily drops to semantic-only instead of risking stale hits.
  Repair is automatic and self-healing on the next recall at the latest.
- The rebuild-token race check may schedule one extra rebuild when
  mirrors race a repair — a deliberate cost for never losing a mirror.
- `apply_async` changed from lazy (`async fn`) to eager dispatch; callers
  are unaffected (`.await` as before), but the batch now always runs even
  if the caller is cancelled.
- Concurrency/ordering stress testing of batched mirrors remains #311.
- Supersedes the failure-handling paragraph of ADR-0038 ("until the next
  startup rebuild" is now "until the background repair converges").
