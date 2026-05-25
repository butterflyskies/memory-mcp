# memory-mcp

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
