# Plugin-defined MCP tools

narwhal v2.0 adds a host-side registration path for MCP tools sourced
from plugins. Built-in tools and plugin-defined tools share the same
discovery (`tools/list`) and dispatch (`tools/call`) surfaces; the
agent sees one unified catalogue.

## Status

- ✅ `ToolRegistry::register_dynamic` — host-side registration API
  with collision handling.
- ✅ `McpServer::with_tools` — accept a pre-populated registry so
  the host can register plugin tools before `serve_stdio` runs.
- ✅ Collision policy: built-in always wins; on dynamic-vs-dynamic
  clashes the first registration wins.
- ⏳ WASM-side WIT bridge (`interface mcp { ... }`) is deferred to
  v2.1. The hook point is the `register_dynamic` API above — the
  v2.1 task is to invoke it from the plugin runtime after calling
  the plugin's `mcp::register-tools` export.
- ⏳ JSON-schema validation of arguments before dispatch is deferred
  to v2.1 (the v2.0 path passes the agent's `arguments` verbatim to
  the handler; the handler can validate at its discretion).

## API surface

```rust
use narwhal_mcp::tools::{DynamicTool, RegistrationOutcome, ToolOutput, ToolRegistry};
use narwhal_mcp::McpServer;
use std::sync::Arc;

let mut registry = ToolRegistry::with_defaults;

let tool = DynamicTool {
  name: "hello_mcp".into,
  description: "Echo the agent's `name` arg back".into,
  input_schema: serde_json::json!({
  "type": "object",
  "properties": { "name": { "type": "string" } },
  "required": ["name"]
  }),
  source: "example-plugin".into,
  handler: Arc::new(|_ctx, args| Box::pin(async move {
  let name = args.get("name").and_then(|v| v.as_str).unwrap_or("anonymous");
  Ok(ToolOutput::ok(format!("Hello, {name}")))
  })),
};

match registry.register_dynamic(tool) {
  RegistrationOutcome::Registered => {}
  RegistrationOutcome::CollisionBuiltin => {
  tracing::warn!("plugin tried to override a built-in tool; ignored");
  }
  RegistrationOutcome::CollisionDynamic { existing_source } => {
  tracing::warn!(?existing_source, "another plugin already registered this tool name");
  }
}

let server = McpServer::with_tools(ctx, registry);
server.serve_stdio.await?;
```

## Collision policy

| First registered | Second registered | Outcome |
|------------------|-------------------|---------|
| Built-in `run_query` | Dynamic `run_query` | `CollisionBuiltin`; the dynamic registration is rejected. |
| Dynamic from plugin-a | Dynamic from plugin-b (same name) | `CollisionDynamic { existing_source: "plugin-a" }`; plugin-a's tool stays. |
| Built-in or dynamic | Different name | `Registered`. |

Built-ins are loaded first via `ToolRegistry::with_defaults`, so the
table above describes every reachable case.

## Roadmap (v2.1)

- WIT world bump 0.1.0 → 0.2.0: add `interface mcp { register-tools, call-tool }`.
- WASM runtime invokes `register-tools` at plugin load and forwards
  the result to `register_dynamic`.
- `call-tool` dispatch path: take the registry's `find` result,
  marshal `tool-call-input` into the plugin, marshal the response
  back as MCP `Content`.
- JSON-schema validation of arguments before dispatch using the
  `jsonschema` crate.
- New `mcp.register` capability gating the `register-tools` export.
- ACL passthrough verification: plugin tools that open connections
  via `host::cmd("open …")` honour the `connection.access` capability.
