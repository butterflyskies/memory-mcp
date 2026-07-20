# Client setup

memory-mcp exposes MCP over Streamable HTTP (the default; endpoint
`http://localhost:8080/mcp`) or, for single-user local setups, over stdio
with `memory-mcp serve --transport stdio`.

Client configuration formats evolve independently from memory-mcp. Treat these
examples as starting points and consult the client documentation when a current
release rejects a field or location.

## Claude Code

Add to `~/.claude.json` or a project's `.mcp.json`:

```json
{
  "mcpServers": {
    "memory": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

## Cursor

Add to `.cursor/mcp.json` for a project or `~/.cursor/mcp.json` globally:

```json
{
  "mcpServers": {
    "memory": {
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

## Visual Studio Code with GitHub Copilot

Add to `.vscode/mcp.json`:

```json
{
  "servers": {
    "memory": {
      "type": "http",
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

Visual Studio Code uses `servers` as the root key rather than `mcpServers`.

## Windsurf

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "memory": {
      "serverUrl": "http://localhost:8080/mcp"
    }
  }
}
```

## Continue

Add `.continue/mcpServers/memory.yaml`:

```yaml
mcpServers:
  - name: memory
    type: streamable-http
    url: http://localhost:8080/mcp
```

## stdio (no daemon)

For single-user local use, let the client manage the server process directly —
no background daemon, port, or Host allowlist needed. Claude Code example:

```json
{
  "mcpServers": {
    "memory": {
      "command": "memory-mcp",
      "args": ["serve", "--transport", "stdio"]
    }
  }
}
```

Each stdio client gets its own server process (and its own embedding model in
memory), and all processes must share one repository sequentially: a second
server against the same repo exits with an error while another is running
(ADR-0040). Prefer the HTTP daemon when more than one client is connected at
a time.
