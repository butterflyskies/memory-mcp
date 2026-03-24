## [0.4.0]

### Added
- Address final review P3 findings
- Address review findings for partitioned indexes
- Add unit tests for ScopeFilter::matches()
- Address code review findings
- Add workflow_dispatch to release workflow by @butterflysky-ai in [#89](https://github.com/butterflyskies/memory-mcp/pull/89)
- Add trusted publishing for crates.io releases by @butterflysky-ai in [#77](https://github.com/butterflyskies/memory-mcp/pull/77)
- Add deployment workstream plan and update project memories by @butterflysky-ai in [#76](https://github.com/butterflyskies/memory-mcp/pull/76)
- Add MCP client config examples for all major editors by @butterflysky-ai in [#75](https://github.com/butterflyskies/memory-mcp/pull/75)
- Add cargo-semver-checks as required CI job by @butterflysky-ai in [#73](https://github.com/butterflyskies/memory-mcp/pull/73)

### Changed
- Bump version to 0.4.0, revert workflow changes
- Scope-partitioned vector indexes
- Scope affinity for recall and list
- Skip redundant verification build in publish-crate by @butterflysky-ai in [#91](https://github.com/butterflyskies/memory-mcp/pull/91)
- Release 0.3.1 by @butterflyskies-release-manager-bot[bot] in [#90](https://github.com/butterflyskies/memory-mcp/pull/90)
- Bounded session management via mcp-session by @butterflysky-ai in [#82](https://github.com/butterflyskies/memory-mcp/pull/82)

### Fixed
- Serialise ScopedIndex add/remove with write lock
- Harden index persistence and remove dead code
- Fix README accuracy and update TODO to reflect current state by @butterflysky-ai in [#74](https://github.com/butterflyskies/memory-mcp/pull/74)

### Removed
- Remove dead ensure_scope, document lock ordering
- Remove release-please workflow and configuration by @butterflysky-ai in [#95](https://github.com/butterflyskies/memory-mcp/pull/95)

## [0.3.0] - 2026-03-21

### Changed
- Release 0.3.0 by @butterflyskies-release-manager-bot[bot] in [#58](https://github.com/butterflyskies/memory-mcp/pull/58)
- Fat lib / thin binary — expose domain modules from lib.rs by @butterflysky-ai in [#64](https://github.com/butterflyskies/memory-mcp/pull/64)
- Update project overview with trust signals and release infrastructure by @butterflysky-ai in [#63](https://github.com/butterflyskies/memory-mcp/pull/63)
- Trust signals phase 1 — metadata, cargo-deny, rustdoc by @butterflysky-ai in [#59](https://github.com/butterflyskies/memory-mcp/pull/59)
- Update project overview and add operational concerns memory by @butterflysky-ai in [#54](https://github.com/butterflyskies/memory-mcp/pull/54)

### Fixed
- Use GitHub App token for release-please to trigger CI by @butterflysky-ai in [#57](https://github.com/butterflyskies/memory-mcp/pull/57)

### New Contributors
* @butterflyskies-release-manager-bot[bot] made their first contribution in [#58](https://github.com/butterflyskies/memory-mcp/pull/58)

## [0.2.0] - 2026-03-20

### Changed
- Release 0.2.0 by @github-actions[bot] in [#53](https://github.com/butterflyskies/memory-mcp/pull/53)
- Replace fastembed with candle direct for pure-Rust embeddings by @butterflysky-ai in [#51](https://github.com/butterflyskies/memory-mcp/pull/51)
- Update project overview memory with cross-platform CI details by @butterflysky-ai in [#47](https://github.com/butterflyskies/memory-mcp/pull/47)

## [0.1.5] - 2026-03-19

### Changed
- Release 0.1.5 by @github-actions[bot] in [#46](https://github.com/butterflyskies/memory-mcp/pull/46)
- Vendor OpenSSL and add cross-platform compilation checks by @butterflysky-ai in [#43](https://github.com/butterflyskies/memory-mcp/pull/43)

### Fixed
- Vendor OpenSSL only on non-Linux platforms by @butterflysky-ai in [#45](https://github.com/butterflyskies/memory-mcp/pull/45)

## [0.1.4] - 2026-03-19

### Changed
- Release 0.1.4 by @github-actions[bot] in [#39](https://github.com/butterflyskies/memory-mcp/pull/39)
- Release 0.1.5 by @github-actions[bot] in [#36](https://github.com/butterflyskies/memory-mcp/pull/36)
- Release 0.1.4 by @github-actions[bot] in [#35](https://github.com/butterflyskies/memory-mcp/pull/35)
- Release 0.1.4 by @github-actions[bot] in [#32](https://github.com/butterflyskies/memory-mcp/pull/32)
- Release 0.1.5 by @github-actions[bot] in [#29](https://github.com/butterflyskies/memory-mcp/pull/29)
- Release 0.1.4 by @github-actions[bot] in [#27](https://github.com/butterflyskies/memory-mcp/pull/27)

### Fixed
- Upgrade release-please-action for force-tag-creation support by @butterflysky-ai in [#38](https://github.com/butterflyskies/memory-mcp/pull/38)
- Restore release-please labels and reset version to v0.1.3 by @butterflysky-ai in [#34](https://github.com/butterflyskies/memory-mcp/pull/34)
- Clean up orphaned release state and skip labeling by @butterflysky-ai in [#31](https://github.com/butterflyskies/memory-mcp/pull/31)
- Move doc comment above #[cfg] so clap shows help for k8s-secret store by @butterflysky-ai in [#28](https://github.com/butterflyskies/memory-mcp/pull/28)
- Use draft releases so binary assets can be uploaded before publish by @butterflysky-ai in [#26](https://github.com/butterflyskies/memory-mcp/pull/26)

## [0.1.3] - 2026-03-18

### Added
- Add release binary assets with SHA256 checksums by @butterflysky-ai in [#22](https://github.com/butterflyskies/memory-mcp/pull/22)

### Changed
- Release 0.1.3 by @github-actions[bot] in [#25](https://github.com/butterflyskies/memory-mcp/pull/25)

### Fixed
- Move doc comments above #[cfg] so clap shows help text for k8s flags by @butterflysky-ai in [#24](https://github.com/butterflyskies/memory-mcp/pull/24)
- Fix cargo-binstall version pin and quiet gh run watch by @butterflysky-ai in [#21](https://github.com/butterflyskies/memory-mcp/pull/21)

## [0.1.2] - 2026-03-18

### Changed
- Release 0.1.2 by @github-actions[bot] in [#20](https://github.com/butterflyskies/memory-mcp/pull/20)

### Fixed
- Fix release image tags and gate publish on CI by @butterflysky-ai in [#19](https://github.com/butterflyskies/memory-mcp/pull/19)

## [memory-mcp-v0.1.1] - 2026-03-18

### Added
- Add comprehensive README by @butterflysky-ai in [#15](https://github.com/butterflyskies/memory-mcp/pull/15)
- Add Kubernetes deployment (Round 1) by @butterflysky-ai in [#13](https://github.com/butterflyskies/memory-mcp/pull/13)
- Add release-please and PR title linting by @butterflysky-ai in [#14](https://github.com/butterflyskies/memory-mcp/pull/14)
- Add --store k8s-secret backend for auth login (#k8s feature) by @butterflysky-ai in [#12](https://github.com/butterflyskies/memory-mcp/pull/12)
- Add keyring-based token storage as auth fallback by @butterflysky-ai in [#9](https://github.com/butterflyskies/memory-mcp/pull/9)
- Add ADR-0010: keyring-based token storage by @butterflysky-ai in [#5](https://github.com/butterflyskies/memory-mcp/pull/5)

### Changed
- Release memory-mcp 0.1.1 by @github-actions[bot] in [#18](https://github.com/butterflyskies/memory-mcp/pull/18)
- Migrate to googleapis/release-please-action and bump action versions by @butterflysky-ai in [#17](https://github.com/butterflyskies/memory-mcp/pull/17)
- Update project overview memory by @butterflysky-ai in [#16](https://github.com/butterflyskies/memory-mcp/pull/16)
- Update project overview and session handoff memories by @butterflysky-ai in [#11](https://github.com/butterflyskies/memory-mcp/pull/11)
- Implement auth subcommand with OAuth device flow by @butterflysky-ai in [#10](https://github.com/butterflyskies/memory-mcp/pull/10)
- Incremental index rebuild on pull by @butterflysky-ai in [#7](https://github.com/butterflyskies/memory-mcp/pull/7)
- Modify funding sources in FUNDING.yml by @butterflysky in [#8](https://github.com/butterflyskies/memory-mcp/pull/8)
- Update TODO.md to reflect Phase 2 progress by @butterflysky-ai in [#6](https://github.com/butterflyskies/memory-mcp/pull/6)
- Implement git push/pull with auth and conflict resolution by @butterflysky-ai in [#4](https://github.com/butterflyskies/memory-mcp/pull/4)
- Update TODO.md to reflect current project status by @butterflysky-ai in [#3](https://github.com/butterflyskies/memory-mcp/pull/3)
- Implement all 7 MCP tool handlers with full observability by @butterflysky-ai in [#2](https://github.com/butterflyskies/memory-mcp/pull/2)
- Scaffold Rust MCP server with streamable HTTP transport by @butterflysky-ai in [#1](https://github.com/butterflyskies/memory-mcp/pull/1)
- Seed project: git-backed semantic memory MCP server by @butterflysky

### New Contributors
* @github-actions[bot] made their first contribution in [#18](https://github.com/butterflyskies/memory-mcp/pull/18)
* @butterflysky-ai made their first contribution in [#17](https://github.com/butterflyskies/memory-mcp/pull/17)
* @butterflysky made their first contribution in [#8](https://github.com/butterflyskies/memory-mcp/pull/8)

