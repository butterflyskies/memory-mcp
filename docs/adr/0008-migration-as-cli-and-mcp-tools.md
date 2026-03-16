# ADR-0008: Migration exposed as both CLI subcommands and MCP tools

## Status
Accepted

## Context
Migration from Serena memories is a one-shot operation for the human operator, but
agents also need to trigger it (e.g., during onboarding or project setup). Choosing
only one exposure mechanism leaves a gap.

## Decision
Expose migration as both CLI subcommands (`memory-mcp migrate serena-global`) and
MCP tools (`migrate_serena_global`, `migrate_serena_project`). The core logic lives
in `src/migrate.rs`; both CLI and MCP tools call the same functions.

## Consequences
- Human operators can run migration from the terminal without a running server.
- Agents can trigger migration within an MCP session.
- Two thin wrappers (CLI + MCP handler) must be maintained, but the shared core
  minimizes duplication.
