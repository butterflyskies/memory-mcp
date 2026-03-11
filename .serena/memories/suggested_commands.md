# Suggested Commands

## Build & Run
```bash
cargo build                          # debug build
cargo build --release                # release build (strip+lto)
cargo run -- --bind 127.0.0.1:3000   # run server locally
```

## Quality Checks (run before every commit)
```bash
cargo fmt                            # format code
cargo clippy -- -D warnings          # lint, treat warnings as errors
cargo nextest run --workspace        # run tests (use nextest, not cargo test)
```

## Full Pre-commit Sequence
```bash
cargo fmt && cargo clippy -- -D warnings && cargo nextest run --workspace && cargo build --release
```

## Git (always use AI identity)
```bash
GIT_CONFIG_GLOBAL=~/.gitconfig.ai git <command>
GH_CONFIG_DIR=~/.config/gh-butterflysky-ai gh <command>
```

## Environment
```bash
export PATH=/home/butterfly/.local/bin:/home/butterfly/.cargo/bin:/home/butterfly/go/bin:/home/butterfly/.asdf/shims:/home/butterfly/.krew/bin:/home/butterfly/.npm-global/bin:/home/butterfly/.local/share/pnpm:/home/butterfly/.local/share/zinit/plugins/zdharma-continuum---null:/home/butterfly/.local/share/zinit/plugins/direnv---direnv:/home/butterfly/.local/share/zinit/plugins/starship---starship:/home/butterfly/.local/share/zinit/plugins/asdf-vm---asdf:/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin
```
