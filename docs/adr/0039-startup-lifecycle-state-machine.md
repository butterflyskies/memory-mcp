# ADR-0039: Startup lifecycle state machine

## Status
Proposed

## Context
PR #308 adds an in-RAM Tantivy BM25 index that rebuilds from the git-backed corpus on every startup. This introduces a new source of startup delay proportional to corpus size. The existing startup flow initialises all subsystems (git repo, embedding engine, vector index, reindex) synchronously before binding the HTTP listener.

In a Kubernetes deployment (the "Goddess cluster"), the pod must pass liveness and readiness probes within configured timeouts. If the lexical index rebuild takes longer than the liveness probe window, the kubelet kills the pod, which restarts and rebuilds again — a crash loop. The problem compounds as the corpus grows.

The existing health infrastructure (ADR-0027) reports subsystem state passively via `SubsystemReporter` but has no concept of boot phases or degraded serving modes. The current `/readyz` handler gates on all subsystems being healthy, including the vector index. Adding a lexical index to this gate means the pod reports not-ready for longer, and a rebuild failure makes the entire server unavailable — even though semantic recall (the pre-#308 capability) is fully functional.

### Design constraint from #310
Issue #310 defines the failure/repair contract for the lexical index: what constitutes divergence from git truth, how divergence is detected, and how repair is triggered. This ADR defines the lifecycle states and probe contracts; #310 defines the state transition triggers on the failure side. The two are complementary: this ADR names the states, #310 defines what makes them change.

## Decision
Introduce a startup lifecycle state machine with degraded-mode serving. The HTTP listener binds before any index work begins, and readiness gates on semantic recall only — never on the lexical index.

### States

```
Booting ──→ SemanticReady ──→ FullyReady
                │                   │
                ▼                   ▼
            Degraded ◄──────── Degraded
                │
                ▼
             Failed
```

| State | Semantic recall | Lexical recall | `/livez` | `/readyz` |
|-------|----------------|----------------|----------|-----------|
| **Booting** | No | No | 200 | 503 |
| **SemanticReady** | Yes | No (rebuilding) | 200 | 200 |
| **FullyReady** | Yes | Yes | 200 | 200 |
| **Degraded** | Yes | No (failed/stale) | 200 | 200 |
| **Failed** | No | No | 503 | 503 |

**Key invariants:**
- The HTTP listener binds and `/livez` returns 200 before any index work starts.
- `/readyz` returns 200 as soon as semantic recall is operational. Lexical index state never gates readiness.
- Lexical index failure degrades recall quality (semantic-only) but does not reduce availability.
- `Failed` is reserved for unrecoverable conditions (git repo corrupt, embedding engine crash) — not for lexical index problems.

### Boot ordering

1. Parse args, init tracing
2. **Bind HTTP listener** — `/livez` returns 200 from this point
3. Open git repo, init embedding engine, load vector index
4. If vector index needs reindex, perform synchronous semantic reindex (existing behaviour)
5. Report `SemanticReady` — `/readyz` returns 200 from this point
6. **Spawn background task**: build lexical index from corpus
7. On lexical completion: transition to `FullyReady`
8. On lexical failure: transition to `Degraded`, log, schedule retry

Steps 1–5 are on the critical startup path. Step 6 is fire-and-forget from the readiness perspective.

### Probe endpoints

**`/livez`** (liveness):
- 200 once the listener is bound and the event loop is running
- 503 only in `Failed` state (unrecoverable)
- Must never fail because an index is slow to rebuild
- Kubernetes `livenessProbe` should use this with a tight interval

**`/readyz`** (readiness):
- 200 once semantic recall is operational (`SemanticReady`, `FullyReady`, or `Degraded`)
- 503 during `Booting` (not yet ready to serve) or `Failed`
- Reports lexical index state in the response body for observability, but does not gate on it
- Kubernetes `readinessProbe` should use this

**`/healthz`** (existing, unchanged):
- Remains a static 200 — legacy compatibility

### Response body extensions

The `/readyz` response gains a `lifecycle` field and a `lexical` check:

```json
{
  "status": "ready",
  "lifecycle": "fully_ready",
  "checks": {
    "git_repo": { "status": "up" },
    "embedding": { "status": "up" },
    "vector_index": { "status": "up" },
    "lexical_index": { "status": "up" },
    "sync": { "status": "up" }
  }
}
```

When degraded:
```json
{
  "status": "ready",
  "lifecycle": "degraded",
  "checks": {
    "lexical_index": { "status": "down", "reason": "rebuild_failed" }
  }
}
```

Note: `status` remains `"ready"` even when `lifecycle` is `"degraded"` — the server can serve traffic, just without lexical recall.

### Kubernetes manifest changes

```yaml
startupProbe:
  httpGet:
    path: /readyz
    port: 8080
  failureThreshold: 30
  periodSeconds: 10
  # 5 minutes for first-boot full reindex of semantic index

livenessProbe:
  httpGet:
    path: /livez
    port: 8080
  periodSeconds: 10
  failureThreshold: 3

readinessProbe:
  httpGet:
    path: /readyz
    port: 8080
  periodSeconds: 5
  failureThreshold: 2
```

The startup probe provides a generous window (5 minutes) for the synchronous semantic reindex on first boot. Once it passes, the tight liveness probe takes over. The lexical rebuild runs after readiness is established — it cannot cause a crash loop regardless of corpus size.

### Integration with #310 failure/repair contract

The state machine consumes events from #310's failure detection:

| Event (from #310) | State transition |
|---|---|
| Lexical rebuild completes successfully | SemanticReady → FullyReady |
| Lexical rebuild fails | SemanticReady → Degraded |
| Drift detected (lexical ≠ git truth) | FullyReady → Degraded, trigger rebuild |
| Rebuild/repair completes after degradation | Degraded → FullyReady |
| Cancellation during rebuild | Remain in current state, schedule retry |
| Semantic subsystem failure | Any → Failed |

The repair path reuses the startup rebuild mechanism: discard the in-RAM index, rebuild from git. This means startup and repair share one code path — tested once, proven twice.

### Alternatives considered

- **Block readiness on full index rebuild:** Current behaviour. Rejected because it creates a crash-loop risk proportional to corpus size and makes the server unavailable for something that only affects recall quality, not correctness.
- **Separate liveness/readiness without a state machine:** Would work for the probe contracts but leaves degraded-mode serving implicit. The state machine makes the serving contract explicit and testable.
- **Persistent lexical index (skip rebuild):** Tracked as #312. Would eliminate the startup delay entirely but introduces a durable derived-index surface with its own consistency/corruption risks. Correct long-term direction; the state machine is needed regardless (for failure recovery) and is the right foundation for it.

## Consequences
- Startup time visible to Kubernetes is reduced to semantic-only init — lexical adds zero to the readiness window
- Corpus growth does not increase crash-loop risk
- Lexical recall is best-effort — consumers must not depend on it for correctness, only for recall quality
- The state machine is the integration surface for #310's failure contract and #312's future persistent index
- `/readyz` response body grows (additive, non-breaking for existing consumers that check only the HTTP status code)
- Two new subsystem reporters needed: `lexical_index` and a lifecycle-state field on `HealthRegistry`
