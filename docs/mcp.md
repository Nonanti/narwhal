# MCP server

narwhal embeds a [Model Context Protocol](https://modelcontextprotocol.io/)
server that exposes your saved connections, schema, and query
execution to any MCP-aware agent (Claude Desktop, Cursor, Zed,
Continue, Aider, custom clients).

The server speaks JSON-RPC 2.0 over stdio. No network listener is
opened.

## Running

```sh
narwhal mcp
```

By default, the server resolves connections from
`~/.config/narwhal/connections.toml` and uses the keyring for
secrets.

Flags:

- `--config <path>` — alternate `connections.toml`
- `--read-only` — refuse every mutation, regardless of per-connection
  flags
- `--connections name1,name2` — restrict to a subset

## Wire-up

### Claude Desktop

`~/.config/Claude/claude_desktop_config.json` (or the macOS
equivalent under `Library/Application Support/Claude/`):

```jsonc
{
  "mcpServers": {
    "narwhal": {
      "command": "narwhal",
      "args": ["mcp"]
    }
  }
}
```

### Cursor

`~/.cursor/mcp.json`:

```jsonc
{
  "mcpServers": {
    "narwhal": {
      "command": "narwhal",
      "args": ["mcp", "--read-only"]
    }
  }
}
```

`--read-only` is recommended for any agent you don't fully trust.

### Zed

`~/.config/zed/settings.json`:

```jsonc
{
  "context_servers": {
    "narwhal": {
      "command": {
        "path": "narwhal",
        "args": ["mcp"]
      }
    }
  }
}
```

## Tools

| Tool              | Purpose                                            |
|-------------------|----------------------------------------------------|
| `list_connections`| Saved connections, with engine and read-only flags |
| `describe_schema` | Schemas, tables, columns, FKs, indexes             |
| `run_query`       | Execute SQL on a named connection                  |
| `get_diagram`     | Render an ER diagram (Mermaid or DOT)              |

Each tool's input schema is published through `tools/list`. Plugins
can register additional tools — see [`plugins.md`](./plugins.md).

## Safety

- Per-connection `read_only = true` is honoured. Mutations are
  refused before the query reaches the driver.
- `confirm_writes = true` connections refuse all writes from MCP
  regardless of the SQL shape (no `YES` prompt over the wire).
- Every executed statement is written to the audit log if a sink is
  configured. See [`audit.md`](./audit.md).
- Query responses are capped at 512 KiB. Larger results are
  truncated with a marker; the client must paginate.
