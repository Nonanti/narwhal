# Plugins

narwhal has two plugin runtimes. Pick the one that matches your trust
boundary.

| Runtime | Trust | Use when |
|---|---|---|
| **Lua**  | trusted (full host access) | personal scripts, internal team automation |
| **WASM** | sandboxed (capability-gated) | shared / third-party plugins |

## Lua (trusted)

Drop a `.lua` file in `~/.config/narwhal/plugins/`:

```lua
narwhal.register_command("rc", "row count", function(table)
  local r = narwhal.sql_run("SELECT count(*) FROM " .. table)
  return tostring(r.rows[1][1])
end)
```

Six worked examples in [`examples/plugins/`](../examples/plugins/).
Lua plugins run in-process with no sandbox — they can do anything the
host process can.

## WASM (sandboxed)

Component-model plugins via `wasmtime` with a capability sandbox:

- 64 MiB memory cap
- 100M fuel (cooperative time slicing)
- 256 KiB plugin-scoped KV store
- No filesystem / network by default — every capability is declared
  in the plugin manifest and enforced by host policy.

See [`plugins/wasm.md`](./plugins/wasm.md) for the manifest schema and
[`plugins/security.md`](./plugins/security.md) for the policy model.

## MCP

The MCP server is not a plugin in the conventional sense — it's a
JSON-RPC interface that exposes the same primitives to external
agents. See [`mcp.md`](./mcp.md).
