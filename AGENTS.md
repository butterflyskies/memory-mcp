# Agent Instructions

## Before you start

Read all ADRs in `docs/adr/` — they capture binding architectural decisions and constraints. Respect them unless explicitly superseded.

## Pre-commit

A `.githooks/` hook runs several checks before every commit. Activate it once per clone:

```sh
git config core.hooksPath .githooks
```

The hook runs four checks in order:

1. **`cargo fmt --check`** — formatting. If rejected, run `cargo fmt` and retry.
2. **`cargo clippy -- -D warnings`** — lint. Fix all warnings before committing.
3. **`cargo doc --no-deps`** — doc warnings (broken intra-doc links, missing public docs). Fix before committing.
4. **`gitleaks protect --staged`** — secrets scan. Development-time dependency; install separately: <https://github.com/gitleaks/gitleaks>. If not installed, the hook warns and continues — it does not block the commit.

Before every commit, also ensure these pass (not enforced by the hook):

1. `cargo test`
2. `cargo check --features k8s`

## Testing

- Full suite: `cargo nextest run --workspace --no-fail-fast`
- Before version bumps: add `--all-features`
- Prefer in-process tests (`tower::ServiceExt::oneshot`) over subprocess tests
- Use `#[tokio::test(start_paused = true)]` for time-dependent tests
- Production code uses `?` / `.map_err()` — never `.unwrap()`. Tests can `.unwrap()`.

## Code style

- `#![warn(missing_docs)]` — all public items need doc comments
- Encode guarantees in the type system: newtypes over primitives, validate at boundaries, use typed versions internally
- `pub(crate)` by default — only `pub` when external consumers need it
- `#[non_exhaustive]` on public enums and structs with fields
- `impl Into<String>` for ergonomic constructors
- No verbose comments in tests — the interface should be legible without them

## Error handling

- Use `MemoryError` variants — `InvalidInput` for user-facing validation, `Internal` for infrastructure
- Propagate with `?`, never panic in production code
- Tracing: span names follow `module.operation`, no sensitive data in spans (R-17)

## API surface

- Constructors over public fields — preserves room to evolve without breaking consumers
- `cargo semver-checks check-release` before any version bump
- Minimize the surface: fewer `pub` items = fewer commitments

## CI

- semver-checks enforced — breaking changes need a version bump
- Changelog in `CHANGELOG.md` must cover everything since the last release tag
- Docs-only changes (markdown, licenses, ADRs) skip CI via path filters

## Releases

- Maintainer (butterflysky) handles merging and releases — never auto-merge or call `gh pr merge`
- Stacked PRs: each concern gets its own change, merged in order

## ADRs

When making architectural or design decisions, write a new ADR in `docs/adr/` following the existing format and sequential numbering. Capture what was decided, alternatives considered, tradeoffs accepted, and why. Use ADRs for decisions where alternatives were seriously weighed, technology/dependency choices, architectural patterns, or security decisions — not every change needs one.

## Code review

After a PR passes checks (tests, clippy, fmt):

1. Run `/code-review` on the branch.
2. Fix all findings (P1, P2, P3).
3. Re-run review. Repeat fix-then-review up to 5 rounds.
4. If findings persist after 5 rounds, surface them for human review.

Code review is required before a PR is considered ready for human review.
