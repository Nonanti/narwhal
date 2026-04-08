# MCP server

narwhal ships a built-in Model Context Protocol server so any MCP
client (Claude Desktop, Continue, Zed AI, custom agents) can drive
the same connections, schemas and queries the TUI uses.

```sh
narwhal mcp                    # JSON-RPC stdio server
```

## Tool surface

The server exposes `list_connections`, `describe_schema`, `run_query`,
`explain_query`, `get_diagram`, and more. Read-only by default — write
access is opt-in per connection via the workspace ACL below.

Full schema in [`docs/plugins/mcp-tools.md`](./plugins/mcp-tools.md).

## Claude Desktop

```jsonc
{
  "mcpServers": {
    "narwhal": { "command": "narwhal", "args": ["mcp"] }
  }
}
```

## Workspace ACL

A repo-local `.narwhal/workspace.toml` (committed alongside your
code) scopes what an agent can see:

```toml
allowed_connections = ["staging"]
allow_writes        = false
```

The agent gets the *intersection* of its own config and the
workspace policy — narrowing only, never widening.

## Auditing

Every database call routed through the MCP server is recorded in the
audit log with `source: "mcp"` so you can replay or attribute any
agent action.

See [`docs/audit.md`](./audit.md) for the JSONL format and rotation
settings.
