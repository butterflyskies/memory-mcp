# Task Completion Checklist

Before committing any changes:

1. `cargo fmt` — format all code
2. `cargo clippy -- -D warnings` — zero warnings
3. `cargo nextest run --workspace` — all tests pass
4. `cargo build --release` — release binary builds
5. Verify binary size hasn't grown unexpectedly
6. Bump version in Cargo.toml for functional changes
7. Use feature branch (never commit directly to main)
8. Use AI git identity: `GIT_CONFIG_GLOBAL=~/.gitconfig.ai`
9. Use AI gh identity: `GH_CONFIG_DIR=~/.config/gh-butterflysky-ai`
