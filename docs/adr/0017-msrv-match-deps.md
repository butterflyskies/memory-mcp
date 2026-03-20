# ADR-0017: MSRV policy — match dependency requirements

## Status
Accepted

## Context
Declaring a Minimum Supported Rust Version (MSRV) signals to downstream users which
toolchain they need. Common policies are N-2 stable (e.g. current minus two releases),
latest stable only, or matching whatever the heaviest dependency requires.

memory-mcp depends on candle, tokenizers, and darling (via schemars). Empirical testing
showed darling 0.23.0 requires Rust 1.88, and getrandom 0.4.2 requires edition 2024
(stabilized in 1.85). These transitive requirements already pin us to a recent toolchain
regardless of our own code's needs.

## Decision
Set MSRV to match the highest requirement among our dependencies (currently 1.88). When
a dependency bumps its MSRV, we follow rather than holding back or pinning older versions.

The MSRV is declared in `Cargo.toml` (`rust-version`) and enforced by a CI job that runs
`cargo check --all-features` on the declared toolchain.

## Consequences
- N-2 is not practical for this project — our deps already require N-4 at best
- Downstream users embedding the library need a reasonably recent Rust (acceptable for
  a project using bleeding-edge ML crates)
- MSRV bumps are driven by `cargo update`, not by our own code changes
- CI catches MSRV regressions before they reach main
