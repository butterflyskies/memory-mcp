# ADR-0026: Channel-based worker thread for embedding engine

## Status
Accepted

## Context
The original embedding engine used `Arc<Mutex<CandleInner>>` with `tokio::task::spawn_blocking`.
A candle-core infinite loop on ARM64 macOS (#194) held the mutex forever, permanently blocking
all semantic operations (#192). The only recovery was `kill -9`, which left the index in an
unrecoverable state (#193).

Alternatives considered:
- **Timeout + AtomicBool dead flag**: Wrapping `spawn_blocking` in `tokio::time::timeout` and
  setting a "dead" flag on timeout. Rejected because the blocking thread keeps running with the
  mutex held — subsequent callers would each block on `arc.lock()` before timing out. The dead
  flag makes the engine permanently unavailable (no self-healing).
- **Timeout + engine recreation**: Dropping the old `Arc<Mutex>` and creating a new engine on
  timeout. Rejected due to complexity (model reload latency, HF cache concerns) and the risk
  of the old thread still running with dangling references.

## Decision
Replace the mutex with a dedicated OS thread that owns `CandleInner` exclusively. Async callers
send `(texts, oneshot::Sender)` via a bounded `mpsc::sync_channel` and await the reply with a
configurable timeout. On timeout, the caller gets an error; the worker finishes its current task,
discards the stale reply (send fails silently), and picks up the next request. `catch_unwind`
in the worker loop catches panics without killing the thread. A `Drop` impl closes the channel
and joins the worker.

## Consequences
- **Self-healing**: timeouts and panics are non-fatal; the engine recovers automatically.
- **No mutex contention**: the worker owns the model exclusively; no shared mutable state.
- **Backpressure**: bounded channel (configurable `--embed-queue-size`) rejects overflow immediately.
- **Breaking API change**: `CandleEmbeddingEngine::new()` now requires `(Duration, usize)` params.
  `CandleEmbeddingEngine` lost `UnwindSafe`/`RefUnwindSafe` auto-trait impls (due to `JoinHandle`
  and `SyncSender` fields).
