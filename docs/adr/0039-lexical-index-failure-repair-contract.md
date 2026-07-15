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
   wherever degradation is observed: at each mutation-mirror failure site,
   from the recall handler (`search::spawn_lexical_repair`), and at
   startup when the initial rebuild fails. Recall serves semantic-only
   for the whole degraded window, including during the rebuild. *Every*
   failure while obtaining repository truth marks the index
   rebuild-required — including a `list_memories` failure **before** the
   `rebuild_from` seam, which would otherwise leave a fresh index falsely
   healthy at 0/0 epochs with no repair trigger. A rebuild that returns
   `Ok` while a raced mirror re-flagged the index is logged as a raced
   outcome, not as a repair receipt.
5. **Cancellation is a non-event.** Two layers make this true. First,
   `apply_async` dispatches its batch to the blocking pool **eagerly**
   (before the returned future is first polled), so dropping that future
   never strands a dispatched batch. Second, every handler runs its
   repository write plus index mirror as a **cancellation-shielded
   mutation unit** (`server::shielded_mutation_unit`): the unit executes
   on a detached task, so dropping the request future (client disconnect,
   timeout) abandons only the response — the git commit and its mirror
   dispatch always run to completion together. Without the shield, a
   request cancelled while awaiting the detached blocking git commit
   could let the commit land while the mirror continuation never ran
   (healthy-but-stale, the forbidden class). This covers remember, edit,
   forget, move, and sync's pull + incremental reindex; move dispatches
   its lexical mirror directly after the git commit, before the embedding
   await, so an embedding failure cannot strand the mirror either. If a
   unit's task itself dies (panic, executor teardown), the index is
   conservatively marked rebuild-required.

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
what the failure-side states mean and exports them explicitly.

**Lifecycle seam.** `LexicalIndex::status()` returns a
`LexicalStatus` enum — the explicit signal the lifecycle surface
consumes, with no error-string inference:

- `LexicalStatus::Failed` — construction failed (disabled index, every
  op errors, repair can never recover it). Also queryable as
  `is_available() == false`. This axis is deliberately explicit because
  `is_degraded`/`is_repairing` are both `false` on a disabled index,
  which is indistinguishable from healthy without it. A disabled index
  that has additionally accumulated divergence events still reports
  `Failed` — construction failure dominates.
- `LexicalStatus::Repairing` — degraded and a repair rebuild holds the
  single-flight slot (maps to lifecycle `Rebuilding`).
- `LexicalStatus::Degraded` — degraded, rebuild required, none running.
- `LexicalStatus::Available` — consistent with git truth as of the last
  converged rebuild.

`is_degraded()`/`is_repairing()` remain available for callers that need
the raw flags. The readiness surface never gates on lexical state.

## Consequences
- Failure-injection tests across all mutation paths (remember, edit,
  forget, move, incremental reindex, startup rebuild) plus cancellation
  are the permanent acceptance proof: no silent divergence, deterministic
  post-repair convergence, semantic-only recall while degraded.
- Handler-level cancellation tests cover every mutation family: each
  drives the real handler, waits until the git commit resolves, aborts
  the request future, and asserts the shielded unit still converges the
  lexical index to git truth. A pre-list failure-injection test proves a
  repository-listing failure at startup degrades a fresh index and that
  recall triggers the repair.
- Mutation handlers spend one extra task spawn per request (the
  shielded unit); the response is abandoned on cancellation but the
  work never is.
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
