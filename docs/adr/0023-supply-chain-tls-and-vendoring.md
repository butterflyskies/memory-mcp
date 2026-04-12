# ADR-0023: Supply chain — TLS consolidation, native lib vendoring, dep minimisation

## Status
Accepted

## Context
A supply chain audit (PR #120, issue #121) identified several concerns in the
dependency tree:

1. **Three TLS implementations**: rustls (our direct reqwest dep), native-tls
   (pulled in by hf-hub and reqwest default features, wrapping openssl on Linux /
   Security.framework on macOS / SChannel on Windows), and openssl-sys (used by
   git2/libgit2 for git transport). The native-tls stack duplicated rustls for
   the same HTTP traffic — two competing TLS implementations handling the same
   protocol with no benefit.

2. **Native C regex**: tokenizers' default features included onig (Oniguruma via
   onig_sys), requiring native C compilation. A pure-Rust alternative
   (fancy-regex) was available as a feature flag.

3. **Inconsistent vendoring**: OpenSSL was vendored on macOS/Windows but used
   system headers on Linux, creating variance between build environments.

4. **Unused/deprecated dependencies**: schemars 0.8 (code used rmcp's re-export),
   serde_yaml (deprecated by maintainer), shellexpand and homedir (single trivial
   call sites each).

Package count was 452 with 30 direct dependencies.

## Decision

### Eliminate native-tls from HTTP path
Set `default-features = false` on hf-hub (use only the `ureq` feature for the sync
download API; ureq defaults to rustls) and on reqwest (use only `json` + `rustls-tls`).
This removes native-tls, hyper-tls, tokio-native-tls, and the openssl crate from the
HTTP path.

**Result: 2 TLS implementations remain** — rustls for all HTTP traffic, and openssl-sys
for git2/libgit2 git transport. These serve non-overlapping purposes and neither is
redundant. openssl-sys cannot be replaced with rustls because it is linked by libgit2's
C code.

### Replace native C regex with pure-Rust alternative
Set `default-features = false` on tokenizers with `features = ["progressbar",
"fancy-regex", "esaxx_fast"]`. This replaces onig (C) with fancy-regex (pure Rust).

### Vendor OpenSSL and statically link zlib on all platforms
Move the OpenSSL vendoring from `[target.'cfg(not(target_os = "linux"))'.dependencies]`
to unconditional `[dependencies]` with `features = ["vendored"]`. Add
`libz-sys = { version = "1", features = ["static"] }`.

This makes builds reproducible regardless of host OS but means distro security patches
(e.g. `apt upgrade openssl`) no longer apply — a crate version bump + rebuild is
required for OpenSSL CVEs. Dependabot monitors for advisory-triggered bumps to
openssl-sys.

### Remove unused and deprecated dependencies
- `schemars 0.8` — removed; code uses `rmcp::schemars` (1.x re-export)
- `serde_yaml` — replaced with `serde_yaml_ng` (maintained fork, identical API)
- `shellexpand` — removed; inlined 10-line tilde expansion using `dirs::home_dir()`
- `homedir` — removed; replaced with `dirs::home_dir()`

### Trim chrono default features
`chrono = { default-features = false, features = ["clock", "serde"] }` — only
`Utc::now()` and DateTime serde are used.

## Consequences
- Package count: 452 → 430 (−22)
- Direct dependencies: 30 → 27 (−3)
- HTTP TLS: single stack (rustls); native-tls eliminated
- Native C compilation: onig eliminated; remaining native deps are libgit2, libssh2
  (bundled), usearch/esaxx (bundled C++), D-Bus (system dynamic link for keyring)
- OpenSSL vendoring trade-off: reproducible builds at the cost of requiring rebuilds
  for CVE fixes (mitigated by Dependabot)
- serde_yaml_ng produces byte-identical YAML output for the Frontmatter types used;
  existing on-disk memories round-trip without migration
- Version bump to 0.5.0 (minor) due to public error type change
  (`serde_yaml::Error` → `serde_yaml_ng::Error` in `MemoryError::Yaml`)
