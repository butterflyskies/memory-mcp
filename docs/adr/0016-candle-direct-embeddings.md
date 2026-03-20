# ADR-0016: Candle direct embeddings

## Status
Accepted — supersedes [ADR-0005](0005-fastembed-configurable-from-start.md)

## Context
fastembed relies on ort-sys, an FFI binding to the ONNX Runtime C++ library. This
dependency blocked cross-platform builds: macOS and Windows CI jobs could not compile
ort-sys without pre-built ONNX Runtime binaries for each target. The ort-candle spike
(PR #50) proved that candle — a pure-Rust ML framework — can run BERT inference, but
required patching the ort-candle backend. Using candle directly removes the ort layer
entirely.

## Decision
Replace fastembed with candle-core, candle-transformers, and the tokenizers crate to
perform pure-Rust BERT inference. The model is BGE-small-en-v1.5, loaded via the
HuggingFace Hub client (hf-hub). Model files are cached under `HF_HOME` (defaults to
`~/.cache/huggingface`).

The embedding model is hardcoded to BGE-small-en-v1.5 for now. Making it configurable
is deferred until there is a concrete need.

## Consequences
- Zero C/C++ FFI for the embedding pipeline — compiles on Linux, macOS, and Windows
- Cross-platform CI (PR #42 blocker for Windows aside from usearch) is unblocked for
  the embeddings side
- Model caching uses HuggingFace Hub conventions (`HF_HOME`), replacing the previous
  `FASTEMBED_CACHE_DIR` / `.fastembed_cache` approach
- Single hardcoded model simplifies configuration; the `--embedding-model` CLI arg from
  ADR-0005 is removed
- Future: if multiple models are needed, add a config option at that point
