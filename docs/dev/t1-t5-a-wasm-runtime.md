# T1-T5-A — WASM plugin runtime

> Status: **landed on v2-dev**. Feeds T3-01 (migration guide) with
> the public-surface delta below. Tier-2 task T2-T5-C (MCP-tool
> plugins) and the follow-up T1-T5-B (sandbox enforcement) build on
> the contract documented here.

## Headline

A second plugin runtime — `narwhal-plugin-wasm` — lands alongside the
existing Lua track. Plugins are component-model `.wasm` files
produced by any language with a `wit-bindgen` toolchain (Rust +
TypeScript via `componentize-js` are documented). The WIT contract is
in `crates/narwhal-plugin-wasm/wit/world.wit`; the host instantiates
each plugin into its own `wasmtime::Store` with memory + fuel
budgets and a per-plugin KV namespace.

The existing Lua plugin track is **unchanged**. Hosts that want both
can register a Lua plugin and a WASM plugin into the same
`PluginRegistry` and every event fans out through both runtimes in
parallel.

## Public surface delta

### `narwhal-plugin`

```
+ pub enum PluginEvent { ConnectionOpened, ConnectionClosed,
+                        QueryStarted, QueryFinished,
+                        EditorBufferChanged }
+ Plugin::on_event(&self, event: &PluginEvent)
      -> impl Future<Output = PluginResult<()>> + Send;     // default no-op
+ PluginRegistry::broadcast_event(&self, event: &PluginEvent)
      -> impl Future<Output = Result<(), TransformErrors>> + Send;
```

The `Plugin::on_event` default returns `Ok(())` so every existing Lua
plugin (and every external implementor) continues to compile without
change. `broadcast_event` follows the same error-collection shape as
`transform_result`: per-plugin failures are aggregated into a single
`TransformErrors` so a buggy plugin can't suppress delivery for any
of the others.

### `narwhal-plugin-wasm` (new crate)

```
+ pub struct Runtime;
+ pub struct RuntimeConfig {
      pub memory_limit: usize,        // default 64 MiB
      pub fuel_per_call: u64,         // default 100 M
      pub kv_budget: usize,           // default 256 KiB
      pub policy: WasmPluginSettings, // borrowed from narwhal-config
  }
+ pub struct WasmPlugin: Plugin;
+ pub struct Manifest;
+ pub struct CapabilitySet;
+ pub enum Capability { State, Cmd, FsRead, FsWrite, Net, Env };
+ pub trait CommandBus;                // host-side injection trait
+ pub trait LogSink;                   // host-side injection trait
+ pub struct HostState;                // wasmtime Store data
+ pub mod bindings;                    // wasmtime::component::bindgen! output
+ pub const HOST_API_MAJOR: u32 = 1;
+ pub const DEFAULT_MEMORY_LIMIT, DEFAULT_FUEL_BUDGET, DEFAULT_KV_BUDGET;
```

Every new `pub struct` is `#[non_exhaustive]` and has a `Default`
impl; consumers extend via field assignment after `default()`. The
two service traits (`CommandBus`, `LogSink`) keep this crate free of
a hard `narwhal-app` dependency — the binary's `AppCore` will inject
the production implementations in a follow-up to T1-T5-A
(`crates/narwhal-app/src/core/plugin_executor.rs` rewires onto the
shared `PluginRegistry::broadcast_event`).

### `narwhal-app`

No public surface delta in T1-T5-A itself. The wiring to actually
hand the runtime an `Arc<dyn CommandBus>` and to dispatch
`PluginEvent` values from the run worker is intentionally left for a
short follow-up: the runtime is testable today (see
`crates/narwhal-plugin-wasm/tests/runtime.rs` — 9 integration tests),
and the app-side wiring touches the same `plugin_executor.rs` file
that the Tier-1 MCP-tool track (T2-T5-C) will edit. Doing it now
would force two merges of the same module — the discipline matches
the Tier-0 / T1-T4-A pattern (defer cross-cutting wiring to a
focused follow-up so paths stay merge-friendly).

## Breaking change envelope

For T3-01's migration guide:

> v2.0 adds a `Plugin::on_event` default method to
> `narwhal_plugin::Plugin`. The default returns `Ok(())`, so
> existing plugin runtimes (Lua, custom out-of-tree) keep compiling
> without change. Runtimes that want event delivery override the
> method; the new `PluginRegistry::broadcast_event` fans the
> `PluginEvent` enum into every registered plugin.
>
> A new `narwhal-plugin-wasm` crate ships in the workspace. It is
> not enabled in the default binary build; consumers wire it up by
> constructing a `Runtime` and registering its `WasmPlugin` outputs
> into the existing `PluginRegistry`. No public type from any other
> crate gained or lost a field.

### Migration recipe for external `Plugin` impls

```rust
// Before (v1.x):
impl Plugin for MyRuntime { /* unchanged */ }

// v2.0: identical — Plugin::on_event has a default Ok(()) body.
// Override only when you want event-driven behaviour.
impl Plugin for MyRuntime {
    /* unchanged */

    async fn on_event(&self, event: &PluginEvent) -> PluginResult<()> {
        // your event-handling logic
        Ok(())
    }
}
```

## Tier-2 contract (T2-T5-C MCP-tool plugins)

T2-T5-C is the Tier-2 task that depends most directly on T1-T5-A's
output. The contract it builds against:

1. **Runtime construction**: T2-T5-C reuses `narwhal_plugin_wasm::Runtime`
   exactly as the v0.1 lifecycle plugins do. A second WIT world
   (`mcp-tool` — to be defined in T2-T5-C) lives alongside the v0.1
   `narwhal-plugin` world in `wit/`. The bindings module re-runs
   `wasmtime::component::bindgen!` for the new world; the engine,
   the linker base, and the capability set are shared.
2. **Capability surface**: MCP-tool plugins get a new `Capability::McpRead`
   token (default-denied). The token is added to the `Capability`
   enum behind `#[non_exhaustive]` so this is a non-breaking
   extension.
3. **Host KV scoping**: MCP-tool plugins share the per-plugin KV
   namespace with their lifecycle counterpart of the same name —
   one TOML manifest, both worlds. The 256 KiB budget applies once
   per plugin name.
4. **Component reuse**: the WIT contract intentionally splits
   `init` / `handle-command` / `on-event` so an MCP-tool world can
   add a fourth export (`list-tools`, `call-tool`) without disturbing
   the v0.1 surface. Components implementing both worlds compose
   into a single `.wasm` and the host instantiates them together.

## Capability boundary (T1-T5-B)

T1-T5-A intentionally implements only the **policy match**: the
manifest declares capabilities, the host's `WasmPluginSettings`
grants them, and the runtime refuses to load a plugin whose declared
set is broader than what the host allows.

The **per-call enforcement** — actually trapping `host.fs_read`
when the policy didn't grant `fs-read`, blocking WASI socket syscalls,
etc. — is **T1-T5-B's** scope. The boundary between the two tasks:

| Surface                                | T1-T5-A | T1-T5-B |
| -------------------------------------- | :-----: | :-----: |
| Manifest parser + capability tokens    |   ✅    |   —     |
| Policy match (`CapabilitySet::check_allowed`) | ✅ |   —     |
| `host.state-*` capability guard        |   ✅    |  hard-trap upgrade |
| `host.cmd` capability guard            |   ✅    |  hard-trap upgrade |
| WASI fs read/write surface             |   —     |   ✅    |
| WASI socket surface                    |   —     |   ✅    |
| WASI env-var surface                   |   —     |   ✅    |
| Audit log of capability denials        |   —     |   ✅    |

T1-T5-A returns capability denials as a polite `ok=false` result
record so plugins can detect the missing wiring; T1-T5-B will flip
those to wasm traps so a hostile plugin can't paper over the denial.

## Resource limits

| Knob                       | Default      | Wasmtime mechanism                     |
| -------------------------- | ------------ | -------------------------------------- |
| Memory per plugin Store    | 64 MiB       | `StoreLimitsBuilder::memory_size`      |
| Fuel per export call       | 100 M ops    | `Store::set_fuel` topped up pre-call   |
| KV byte budget per plugin  | 256 KiB      | Host-side check inside `state-set`     |

`RuntimeConfig` carries the knobs; `WasmPlugin::deliver_event` and
`WasmPlugin::dispatch` re-fuel before every export call so each
event invocation gets a fresh budget. A plugin that overspends
memory is killed by wasmtime's `Trap::MemoryOutOfBounds` and the
host surfaces a `PluginError::Runtime` (delivery to other plugins
continues — see `PluginRegistry::broadcast_event`).

## Acceptance criteria status

| Item                                              | Status |
| ------------------------------------------------- | :----: |
| `narwhal-plugin-wasm` crate builds                |   ✅   |
| `world.wit` documented and stable for v2.0        |   ✅   |
| `examples/hello-world/` builds to `.wasm`         |   ✅ (stand-alone via `cargo component`) |
| Plugin loads + logs on every event                |   ⏳ end-to-end test gated on `NARWHAL_WASM_EXAMPLE` (no component-toolchain in CI baseline yet) |
| Memory + fuel limits enforced                     |   ✅ (wasmtime mechanism; oversize-allocation test requires a guest component → integration sweep) |
| Both Lua and WASM plugins receive event stream    |   ✅ (`PluginRegistry::broadcast_event`; T1-T5-A unit test covers both runtimes registered together) |
| Plugin failure isolated                           |   ✅ `broadcast_event` collects per-plugin errors |
| WASM plugins load from `${config}/plugins/wasm/`  |   ⏳ app-side wiring follow-up (deferred per the "files to touch" boundary) |
| Plugin SDK doc in `docs/plugins/wasm.md`          |   ✅   |
| Definition of Done passes                         |   ✅ (fmt, clippy -D warnings, rustdoc -D warnings, all tests dev+release; 1041 tests vs Tier-0's 984 — 57 net additions) |

The two ⏳ items both reflect the same boundary cut: the app-side
wiring (loading `.wasm` files from `$XDG_CONFIG_HOME/narwhal/plugins/wasm/`,
dispatching `PluginEvent` from the run worker, providing the
`CommandBus` implementation) is a single focused follow-up to land
before v2.0 GA. T1-T5-A ships the *runtime* and the *contract*; the
follow-up plugs the runtime into `AppCore` and lights up the
end-to-end test.

## Convention notes for the next Tier-1 / Tier-2 agent

- Every new public struct in `narwhal-plugin-wasm` is
  `#[non_exhaustive]` + `Default` + field-assignment for
  construction. Struct literal usage is *forbidden* from outside the
  crate; see `RuntimeConfig::default()` for the canonical pattern.
- `Capability` is `#[non_exhaustive]` — every match arm needs a
  defensive `_ =>` arm with `debug_assert + tracing::error + "open an
  issue"`, mirroring the workspace convention in
  `narwhal-core::cancel` and the Settings v2 schema.
- The WIT file (`wit/world.wit`) is **append-only** across v2.x.
  Field and variant additions are non-breaking by component-model
  semantics; renames or removals require an `api-version` bump and a
  T3-01 entry.
- Host functions live in `src/host.rs` and follow the wasmtime 45
  `bindgen!` convention: import functions return
  `wasmtime::Result<T>` so capability denials can later become traps
  (T1-T5-B) without breaking the trait shape.
- `WasmPlugin` clones via shared `Arc` (the wasmtime Store stays
  behind a `tokio::sync::Mutex` — every export call serialises
  through it). External code that needs concurrent access spawns
  separate plugin loads, not parallel calls into one Store.
