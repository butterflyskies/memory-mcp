# Agent Instructions

## Authority and orientation

- Explicit maintainer direction for the current task supersedes repository guidance. If they conflict, surface the conflict and update the durable guidance; do not silently choose the older rule.
- Read the ADRs applicable to the area you are changing. Respect their status: **Accepted** decisions are binding, **Proposed** decisions are context, and **Superseded** decisions are historical. If an Accepted ADR conflicts with current code, workflows, or maintainer direction, flag it for reconciliation.

## Pre-commit

A `.githooks/` hook runs local checks before every commit. Activate and verify it once per clone:

```sh
git config core.hooksPath .githooks
git config --get core.hooksPath
```

The hook runs, in order:

1. `cargo fmt -- --check`
2. `cargo clippy -- -D warnings`
3. `cargo doc --no-deps`, rejecting warnings
4. `gitleaks protect --staged` when `gitleaks` is installed; otherwise it warns and continues

## Verification

- Canonical full suite: `cargo nextest run --workspace --no-fail-fast`. It uses the repository's nextest configuration; do not substitute `cargo test` for the full-suite receipt.
- For code changes, run the feature combinations affected by the change. Before declaring a code-changing PR ready, cover default and all-features; mirror the full CI feature matrix when changing feature-gated behavior.
- Use in-process tests (`tower::ServiceExt::oneshot`) for handler behavior. Use subprocess tests when process startup or shutdown is the behavior under test.
- Use `#[tokio::test(start_paused = true)]` for deterministic timer, timeout, and backoff semantics when compatible. Use bounded real-time polling for multi-thread scheduling or convergence behavior that paused time cannot represent.
- Tests must be non-vacuous: name the regression that would turn each important test red.
- Dependency or feature changes must preserve the declared MSRV, pass applicable `cargo deny` checks, and preserve the TLS, vendoring, and dependency constraints in the relevant Accepted ADRs.

## Code style

- `#![warn(missing_docs)]` — all public items need doc comments.
- Encode invariants in the type system: prefer domain newtypes over primitives, validate at boundaries, and use typed values internally.
- Default to `pub(crate)`; every `pub` item is a semver commitment and needs an external consumer.
- Public enums are exhaustive by default. Adding a variant is an intentional breaking interface change so downstream exhaustive matches fail to compile. Use `#[non_exhaustive]` only when that specific API explicitly chooses forward-compatible matching. Treat public structs separately based on their contract.
- Prefer `impl Into<String>` for constructor parameters that take ownership of string data; do not apply it mechanically to every API.
- Test comments should explain non-obvious contracts, race windows, or failure models. Do not narrate obvious mechanics or preserve historical trivia in canonical code.

## Error handling and tracing

- Use the most specific `MemoryError` variant. `InvalidInput` is for caller validation failures; `Internal` is only for unexpected failures without a better variant.
- Propagate recoverable input, I/O, protocol, and dependency failures with typed errors.
- `expect` or `unreachable!` is reserved for documented internal invariants or unrecoverable poisoned state where continuing could propagate corruption. Never panic for a recoverable external failure.
- Tracing follows `docs/design/tracing/requirements.md`: span names use `module.operation` (R-02); never record tokens, credentials, API keys, full memory content, or unredacted credential-bearing URLs (R-16–R-18).

## API surface and semver

- Use constructors for validated or invariant-bearing domain types. Public fields are acceptable for intentionally data-like wire DTOs and transparent result aggregates.
- Minimize the public surface: fewer public items mean fewer compatibility commitments.
- For a public-API change, run semver checks with the same feature coverage as CI (default plus explicit `k8s`). Intentional breaking changes require explicit maintainer approval, the appropriate version bump, and migration/changelog material.

## CI and pull requests

- Required CI runs on every PR, including docs-only changes. The workflows are authoritative for the current feature, MSRV, audit, semver, and cross-platform matrix.
- PR titles must satisfy `.github/workflows/lint-pr.yml`: use an allowed Conventional Commit type, use `!` for a breaking change, and do not use angle brackets.
- For GitHub Actions changes, pin third-party actions to full commit SHAs, preserve deny-by-default and least-privilege permissions, and never run untrusted PR code through `pull_request_target`.
- A semver-check failure is a gate, not a ban on intentional breaking changes. Approved breaks ship with the correct version transition.

## Releases

- The maintainer owns merges and version-bump approval. Never merge, tag, publish, or invoke a release command without explicit authorization.
- After an approved version bump reaches `main` and CI passes, workflows create the tag and publish the release artifacts automatically.
- A version-bump/release change updates `CHANGELOG.md` for everything since the previous tag.
- Stacked PRs keep separate concerns in separate PRs and merge in dependency order against the correct base branches.

## ADRs

For a design decision where alternatives were seriously weighed, add an ADR in `docs/adr/` using the next available number and existing format. Record the decision, alternatives, tradeoffs, and consequences. When policy changes, supersede or amend the affected ADR so stale Accepted text does not remain binding.

## Code review

1. The implementer runs a review-fix loop before announcing the change.
2. A sibling performs the independent certification review using the review capability available in their environment.
3. Triage every finding. Fix accepted findings; record and escalate genuine disagreements instead of making a harmful change to satisfy a review counter.
4. Any artifact or governing-instruction change invalidates the prior review receipt. Re-review the exact new head.
5. Repeat until an independent review of the exact current head reports zero findings at or above Low severity.

The maintainer remains the merge latch. A green test run or an author's self-review is not merge authorization.
