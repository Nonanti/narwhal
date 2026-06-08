# Plugins

narwhal has two plugin runtimes:

- **Lua** (via `mlua`) — fast, in-process, scripting flavour. Best
  for snippets, exporters, and result-pane transforms.
- **WASM** (via `wasmtime`, component model) — sandboxed, capability
  gated. Best for plugins you do not fully trust.

Both runtimes register through a single `PluginRegistry`. Events
fan out to every loaded plugin.

## Loading

Plugins live under `~/.config/narwhal/plugins/` by default. Enable
auto-loading in `config.toml`:

```toml
[plugins]
auto_load = true
plugins_dir = "~/.config/narwhal/plugins"
```

Each directory entry is a single file:

- `*.lua` — Lua script
- `*.wasm` — WASM component, paired with a `<name>.toml` manifest

## Lua

A minimal plugin:

```lua
-- ~/.config/narwhal/plugins/uppercase.lua
return {
  name = "uppercase",
  description = "Uppercase the current selection",
  commands = {
    upper = function(ctx)
      ctx.replace_selection(string.upper(ctx.selected_text()))
    end,
  },
}
```

Run with `:upper`.

### Sandbox

The default sandbox is `Restricted`. The `io`, `os`, `package`,
`debug`, and `ffi` modules are absent. A `Restricted` script cannot
read or write files, spawn processes, or escape via the debug
registry.

If you embed `narwhal-plugin-lua` and want to trust a script to
native-code level, opt in explicitly:

```rust
use narwhal_plugin_lua::{LuaPlugin, LuaSandbox};

LuaPlugin::from_path_with_sandbox(path, LuaSandbox::Permissive)?;
```

`narwhaldb`'s auto-loader uses the safe default. User plugins that
import `io` or `os` will load but fail at dispatch with
`attempt to index a nil value (global 'io')`.

## WASM

Plugins are component-model `.wasm` files produced by any toolchain
with `wit-bindgen` support. Rust and TypeScript
(`componentize-js`) are documented in the WIT world at
`crates/narwhal-plugin-wasm/wit/world.wit`.

Every plugin ships with a manifest:

```toml
# uppercase.toml
name = "uppercase"
version = "0.1.0"
description = "Uppercase the current selection"
api = "0.1"
capabilities = []
```

### Capabilities

WASM plugins are denied filesystem, network, and environment access
by default. To grant a scope, list it in the manifest:

```toml
capabilities = [
  "fs.read:/etc/narwhal/templates",
  "net.connect:hooks.example.com:443",
  "env.read:HOME",
]
```

The host re-validates every operation at call time. A plugin that
declares `fs.read:/safe` cannot escape into `/etc/passwd` even if
the runtime asks it to.

### Resource limits

| Limit       | Default   | Source                              |
|-------------|-----------|-------------------------------------|
| Memory      | 64 MiB    | `wasmtime::StoreLimits`             |
| Fuel        | 1 B units | `wasmtime::Store::add_fuel`         |
| KV storage  | 1 MiB     | Per-plugin namespace, enforced host-side |

Overruns trap the plugin. The host surfaces a status-bar message
and leaves other plugins untouched.

## Plugin-defined MCP tools

A plugin can register a tool that becomes visible to MCP clients
the next time `tools/list` runs. See
[`mcp.md`](./mcp.md) for the wire side and the WIT contract in the
crate's `world.wit` for the plugin side.

## Examples

The `examples/plugins/` directory ships:

- `csv_export.lua` — exports the current results to CSV via
  `io.open`. Loaded with `Permissive` sandbox.
- `snippet_store.lua` — a `:snip` command palette backed by a plain
  text file.

Run them by symlinking into your `plugins_dir`.
