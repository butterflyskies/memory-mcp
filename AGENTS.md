# Agent Instructions

Instructions for AI coding agents working in this repository.

## Pre-commit hook

This repo ships a pre-commit hook in `.githooks/` that runs `cargo fmt --check`
before every commit. Git does not use this directory by default — it must be
configured once per clone:

```sh
git config core.hooksPath .githooks
```

**Before your first commit, verify the hook is active:**

```sh
git config core.hooksPath
```

If this prints `.githooks`, you're set. If it prints nothing or a different path,
run the config command above.

The hook prevents formatting issues from reaching CI. If a commit is rejected by
the hook, run `cargo fmt` and re-commit.

## Pre-commit checklist

Before every commit, ensure:

1. `cargo fmt --check` passes (enforced by hook above)
2. `cargo clippy -- -D warnings` passes
3. `cargo test` passes
4. `cargo check --features k8s` compiles (feature-gated code)
