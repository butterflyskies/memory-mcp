# ADR-0011: CLI subcommand structure

## Status
Accepted

## Context
The binary needs to support both server operation and interactive CLI
commands (auth login, auth status, future migration tools). A flat
argument struct cannot cleanly accommodate distinct operational modes.

## Decision
Restructure the CLI into subcommands: `serve` (default, runs the MCP
server), `auth login`, `auth status`. Bare `memory-mcp` with no
subcommand defaults to `serve` with default args. This is a breaking
change for `memory-mcp --bind ...` which becomes `memory-mcp serve --bind ...`.

## Consequences
- Pre-1.0 breaking change to CLI invocation — acceptable, no stable API yet
- Clean extensibility for future subcommands (migrate, index, etc.)
- Server-specific flags are scoped to `serve`, keeping `auth` args separate
