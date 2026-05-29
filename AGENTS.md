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

## Testing

- `cargo nextest run --workspace --no-fail-fast` for the full suite
- `cargo nextest run --workspace --no-fail-fast --all-features` before version bumps
- Production code uses `?` / `.map_err()` — never `.unwrap()`. Tests can `.unwrap()`.
- Prefer in-process tests (`tower::ServiceExt::oneshot`) over subprocess tests
- Use `#[tokio::test(start_paused = true)]` for time-dependent tests

## CI

- semver-checks enforced — breaking changes need a version bump (`cargo semver-checks check-release` to verify locally)
- Changelog in `CHANGELOG.md` — must cover everything since the last release tag
- Docs-only changes (markdown, licenses, ADRs) skip CI via path filters

## Code style

- `#![warn(missing_docs)]` — all public items need doc comments
- If a guarantee can live in the type system, put it there — newtypes over primitives, validate at boundaries, use the typed version internally
- `pub(crate)` by default — only promote to `pub` when external consumers need it
- `#[non_exhaustive]` on public enums and structs with fields
- `impl Into<String>` for ergonomic constructors
- No verbose comments in tests — the interface should be legible without them
- `cargo fmt` before every push

## Error handling

- Use `MemoryError` variants — `InvalidInput` for user-facing validation, `Internal` for infrastructure failures
- Propagate with `?`, never panic in production code
- Tracing: span names follow `module.operation` pattern, no sensitive data in spans (R-17)

## API surface

- Constructors over public fields — preserves ability to evolve without breaking consumers
- Run `cargo semver-checks check-release` before any version bump
- Minimize the surface: fewer `pub` items = fewer commitments

## Releases

- Maintainer (butterflysky) handles merging and releases
- Never auto-merge or call `gh pr merge`
- Stacked PRs: each concern gets its own change, merged in order

## Architecture Decision Records

### Reading ADRs
Before starting any implementation work, read all ADRs in `docs/adr/` to understand prior architectural decisions and constraints. These capture the "why" behind design choices and must be respected unless explicitly superseded.

### Writing ADRs
When making new architectural or design decisions during implementation, write a new ADR in `docs/adr/` following the existing format and sequential numbering. An ADR should capture:
- What was decided
- What alternatives were considered
- What tradeoffs were accepted
- Why this choice was made

Not every change needs an ADR — use them for decisions where alternatives were seriously considered, technology/dependency choices, architectural patterns, or security decisions.

## Code Review

After a PR is ready (tests pass, clippy clean, formatted):

1. If a `code-review` skill exists (check with `/code-review`), use it to review the branch.
2. **Fix all findings** from the review — every severity level (P1, P2, P3).
3. After fixing, run the code review again.
4. Repeat the fix → review cycle until there are **zero remaining findings**, up to **5 rounds**.
5. If findings persist after 5 rounds, stop and surface the remaining findings for human review.

Code review is a required step before a PR is considered ready for human review.
