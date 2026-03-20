# Operational Concerns for Future Deployment Hardening

## mmap'd model weights vs external cache mutation

**Context:** `CandleEmbeddingEngine` memory-maps the safetensors weights file from the HF Hub cache (`$HF_HOME/hub/...`). The mmap is held for the lifetime of the process.

**Risk:** If an external actor (e.g., `huggingface-cli cache`, manual `cp`, concurrent warmup process) modifies or deletes the cached weights file while the server is running, the mmap'd region becomes undefined behavior — potentially SIGBUS or silent data corruption.

**Mitigations considered:**
- `File` handle: prevents deletion on Linux (refcount) but NOT modification — insufficient
- `flock()` advisory locks: only works if all actors cooperate; hf-hub doesn't use them
- Copy-on-read: defeats the purpose of mmap (doubles memory usage for ~130MB weights)

**Recommendation:** Document as an operational constraint: "Do not run `huggingface-cli cache` or mutate the HF cache directory while the server is running." When we build operational runbooks for production deployment, include this. Consider a startup check that takes a snapshot hash of the weights file and periodically verifies it hasn't changed (detect-not-prevent).

**Source:** PR #51 code review round 5 finding, 2026-03-20.
