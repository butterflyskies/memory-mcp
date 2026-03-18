# Code Review: CI Workflow Files (2026-03-18)

Scope: `.github/workflows/release.yml`, `.github/workflows/lint-pr.yml`
Branch: `ci/release-automation` (PR #14)

## Findings addressed

### P2: Missing `revert` conventional commit type
`revert:` is a standard conventional commit type recognized by release-please.
Without it in the linter's `types` list, a revert PR would be blocked — forcing
the developer to misrepresent it as `fix:` (which incorrectly triggers a patch
bump) or `chore:` (which suppresses it from the changelog entirely).

**Fix:** Added `revert` to the types list in `lint-pr.yml`.

### P2: `pull_request_target` is a latent privilege escalation footgun
All three independent review sub-agents flagged this. `pull_request_target` runs
in the base branch context with base-repo secrets. Currently safe because the
action only reads the PR event payload (no checkout). But if anyone later adds a
checkout step, fork PRs would execute attacker-controlled code with repo-scoped
tokens. `pull_request` is sufficient — the action reads the PR title from the
event payload, which is available under either trigger.

**Fix:** Changed trigger from `pull_request_target` to `pull_request`. Added
security comment explaining why.

### P3: release-please manifest config
release-please v4 prefers manifest-driven config. Without it, legacy
single-package mode silently ignores workspace members. Adding manifest files
now makes behavior explicit and future-proofs for workspace growth.

**Fix:** Added `release-please-config.json` and `.release-please-manifest.json`.
Updated `release.yml` to reference them instead of inline `release-type`.

### P3: Document linter/release-please coupling
release-please generates PR titles like `chore(main): release X.Y.Z`. The linter
accepts this because `chore` is in the types list and `requireScope: false`. This
coupling is invisible without documentation.

**Fix:** Added inline comment in `lint-pr.yml` documenting the dependency.

### P3: `subjectPattern` accepts HTML/markdown in PR titles
`^.+$` permits angle brackets that flow into auto-generated changelogs. GitHub
sanitizes HTML in release bodies (not XSS), but broken formatting in changelogs
is still undesirable.

**Fix:** Tightened pattern to `^[^<>]+$` with updated error message.

## Findings dropped (with rationale)

### "No build.yml exists" — False positive
Flagged because `build.yml` doesn't exist on this branch. It exists on the
parallel `k8s-deployment-round1` branch (PR #13) and will land on main when that
PR merges. The two PRs are intentionally independent.

### "Direct pushes to main bypass lint" — Not a workflow issue
This is a branch protection configuration concern. If admins bypass branch
protection, that's a deliberate override — no workflow can prevent it. The
mitigation is enabling "Do not allow bypassing the above settings" in branch
protection rules, which is a repo settings change, not a workflow change.

### "`chore!` triggers a major version bump" — By design
Any conventional commit type with `!` suffix signals a breaking change and
triggers a major bump. This is the spec (conventionalcommits.org), not a bug.
A developer writing `chore!:` should understand they're declaring a breaking
change. release-please and the lint workflow are both correct here.
