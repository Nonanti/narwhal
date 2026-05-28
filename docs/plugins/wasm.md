# WASM plugin SDK (v0.1)

> Status: **stable for narwhal 2.0.x**. The WIT contract in
> [`crates/narwhal-plugin-wasm/wit/world.wit`][wit] is append-only
> across the 2.x line — fields and variants may be added but never
> reshaped or removed. Bumping the contract requires an
> `api-version` jump (see `HOST_API_MAJOR`); plugins built against
> an older minor keep loading.

WASM plugins extend narwhal alongside the existing Lua plugin track.
A plugin is one component-model `.wasm` file plus a `plugin.toml`
manifest, dropped into `$XDG_CONFIG_HOME/narwhal/plugins/wasm/`.
Any language with a `wit-bindgen` toolchain can produce one; this
doc walks through Rust and TypeScript, plus the conceptual model
that's the same in every language.

## Conceptual model

A plugin has three lifecycle hooks:

| Hook  | When the host calls it  | Returns  |
| ----------------- | ------------------------------------------------------------------- | -------------------------------- |
| `init`  | Once, immediately after `cargo component`-produced bytes load.  | `result<plugin-info, string>`  |
| `handle-command`  | Every `:` prompt invocation whose name appears in `plugin.toml`.  | `result<command-outcome, string>`|
| `on-event`  | Every host lifecycle event (connection open/close, query, buffer).  | `result<_, string>`  |

The host exposes a tiny capability surface via the imported `host`
interface — see `wit/world.wit` for the canonical IDL. v0.1 ships:

| Host function  | Capability declared in manifest | Notes  |
| ------------------------------ | ------------------------------- | ---------------------------------------------- |
| `log(level, message)`  | none  | Always available; goes through host `tracing`. |
| `cmd(name, args)`  | `cmd`  | Dispatches a `:` command on the host bus.  |
| `state-get(key)`  | `state`  | Returns `none` when the key is unset.  |
| `state-set(key, value)`  | `state`  | Per-plugin KV; 256 KiB byte budget.  |

Capabilities not granted by the host's `[plugins.wasm]` settings
make the *manifest* fail to load — by the time the component runs,
its declared capabilities are guaranteed to be allowed.

## Manifest schema

```toml
# Required: identity and ABI contract.
name  = "my-plugin"  # KV namespace + log tag + Plugin::name
version  = "0.1.0"  # informational; not parsed as semver
api-version = 1  # narwhal:plugin major; must equal HOST_API_MAJOR

# Optional: where the .wasm sits relative to this file.
# Defaults to "<name>.wasm" in the same directory.
component  = "my_plugin.wasm"

# Optional: short user-facing line for `:help`.
description = "Greets connections on open"

# Capabilities the plugin needs. Each must be granted by the host
# `[plugins.wasm]` settings or the plugin is refused at load time.
# Tokens: state, cmd, fs-read, fs-write, net, env.
# Defaults to []; v0.1 only `state` and `cmd` are wired host-side
# (fs-read / fs-write / net / env land with the sandbox).
capabilities = ["state", "cmd"]

# `:` commands the plugin handles. Must not shadow a built-in name.
[[commands]]
name  = "say-hi"
description = "Send a greeting log line"
```

## Resource limits (defaults)

| Knob  | Default  | Source  |
| -------------------------- | ------------ | -------------------------------------- |
| Memory  | 64 MiB  | `RuntimeConfig::memory_limit`  |
| Fuel per export call  | 100 M ops  | `RuntimeConfig::fuel_per_call`  |
| KV byte budget  | 256 KiB  | `RuntimeConfig::kv_budget`  |

Exceeding any of these traps the plugin cleanly. The host logs the
failure but keeps delivering events to every other loaded plugin.

## Rust track

Stand-alone (the host workspace pins `wasmtime` strictly; mixing
`wit-bindgen` versions across workspaces invites confusing
mismatches).

```bash
cargo install cargo-component
```

The example in
[`crates/narwhal-plugin-wasm/examples/hello-world/`][example] is the
minimal Rust plugin. Key pieces:

```toml
# Cargo.toml
[package]
name = "hello-world"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.34"

[package.metadata.component]
package = "narwhal:hello-world"

[package.metadata.component.target]
path = "wit"  # copy the host's wit/world.wit here
world = "narwhal-plugin"
```

```rust
// src/lib.rs
#![no_std]
extern crate alloc;

wit_bindgen::generate!({
  path: "wit",
  world: "narwhal-plugin",
});

use alloc::format;
use alloc::string::String;

use exports::narwhal::plugin::plugin::{CommandInput, CommandOutcome, Event, Guest, PluginInfo};
use narwhal::plugin::host;

struct Plugin;

impl Guest for Plugin {
  fn init -> Result<PluginInfo, String> {
  host::log("info", "initialising");
  Ok(PluginInfo {
  name: String::from("my-plugin"),
  version: String::from("0.1.0"),
  api_version: 1,
  })
  }

  fn handle_command(input: CommandInput) -> Result<CommandOutcome, String> {
  Ok(CommandOutcome::Status(format!("hi {}", input.argument)))
  }

  fn on_event(event: Event) -> Result<, String> {
  if let Event::ConnectionOpened(name) = event {
  host::log("info", &format!("hello, {name}"));
  }
  Ok()
  }
}

export!(Plugin);
```

Build with `cargo component build --release`. The output is
`target/wasm32-wasip1/release/my_plugin.wasm`.

## TypeScript track (stub)

`componentize-js` turns a JavaScript module into a WIT-conformant
component. The exact integration is still in flux upstream; the
recipe below is the canonical setup as of writing.

```bash
npm i -D @bytecodealliance/componentize-js
```

```ts
// plugin.ts — pseudo-code; check componentize-js docs for the
// exact import path. The `host` import is provided by narwhal.
import { log } from "narwhal:plugin/host@0.1.0";

export function init {
  log("info", "initialising");
  return { name: "my-plugin", version: "0.1.0", apiVersion: 1 };
}

export function handleCommand(input) {
  return { tag: "status", val: `hi ${input.argument}` };
}

export function onEvent(event) {
  if (event.tag === "connection-opened") {
  log("info", `hello, ${event.val}`);
  }
}
```

```bash
componentize-js plugin.ts --wit ../narwhal-plugin-wasm/wit \
  --world narwhal-plugin -o my_plugin.wasm
```

Drop the resulting `my_plugin.wasm` next to its `plugin.toml`.

## Debugging

* `host::log("debug", "...")` is the canonical interactive debugger.
  Lines are tagged with the plugin name in the host's `tracing`
  output (`~/.cache/narwhal/logs/narwhal.log`).
* Component-model traps surface with the plugin name in the host
  log; rebuild the plugin with debug symbols to widen the trace.
* Cold-start instantiation is ~5-10 ms per plugin. v0.1 instantiates
  on first load and reuses the Store for every subsequent event;
  hot reload is not supported (restart narwhal to reload a plugin).

## Versioning

The `narwhal:plugin` package and the contained WIT interfaces are
versioned via the `api-version` field on `plugin-info`. The host
refuses any component whose major part of `api-version` does not
equal `HOST_API_MAJOR` (1 in v0.1). When a v2.x minor release adds
fields or variants, the on-the-wire layout stays backwards-compatible
under the component model; only major bumps reshape the surface.

## Where to go next

* The WIT contract: [`crates/narwhal-plugin-wasm/wit/world.wit`][wit]
* The runtime contract notes:
  [`docs/dev/wasm-runtime.md`](../dev/wasm-runtime.md)
* The Rust example:
  [`crates/narwhal-plugin-wasm/examples/hello-world/`][example]

[wit]: ../../crates/narwhal-plugin-wasm/wit/world.wit
[example]: ../../crates/narwhal-plugin-wasm/examples/hello-world/
