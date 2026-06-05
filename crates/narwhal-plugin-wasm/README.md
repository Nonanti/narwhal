# narwhal-plugin-wasm

WebAssembly component-model plugin runtime for [narwhal][nw], landed as part
of the v2.0 roadmap (task **T1-T5-A**). Ships alongside the existing
[`narwhal-plugin-lua`][lua] track so the host can load both at the same time
and fan every lifecycle event into either.

## What it is

A library that turns a `.wasm` component plus a `plugin.toml` manifest into a
`narwhal_plugin::Plugin` trait object the surrounding `PluginRegistry` can
host. Plugins are produced by any toolchain that targets the
[component model][cm]; the included `examples/hello-world` walks through the
Rust + `wit-bindgen` track and `docs/plugins/wasm.md` adds a TypeScript stub
via `componentize-js`.

## v0.2 surface (T1-T5-B)

- `Runtime` / `RuntimeConfig` — process-wide engine + per-plugin policy.
- `Manifest` — schema for `plugin.toml`; checked before any wasm is read.
- `WasmPlugin: narwhal_plugin::Plugin` — one loaded component.
- `CommandBus` / `LogSink` / `AuditSink` — traits the host implements so
  this crate does not pull in `narwhal-app`.
- `Capability` — argument-carrying tokens (`fs.read:/etc`,
  `net.connect:host:port`, ...). Manifest-declared tokens are intersected
  against `Grants` at load time and enforced on every host-fn call by the
  `Enforcer`. Denials emit structured audit events under the tracing
  target `narwhal::plugin::audit` and become wasmtime traps so plugins
  cannot paper over the denial.
- `Operation` / `Decision` — the per-call vocabulary the enforcer
  evaluates.
- `DecisionCache` — hot-path cache keyed on `(plugin, operation)`. The
  first denial audits; subsequent identical denials reuse the original
  `audit_id`.
- `FuelMeter` / `KvAccount` / memory-limit helpers — see `src/limits/`.
- Legacy bare tokens (`fs-read`, `net`, `env`, `fs-write`) still parse
  for T1-T5-A compatibility and expand to the widest scope of their
  kind; new manifests should prefer the explicit form.

## Security model

Documented in detail in `docs/plugins/security.md` and
`docs/dev/t1-t5-b-sandbox.md`. Headlines:

- **Default-deny.** Empty settings refuse every FS / net / env
  capability. Operators opt in per-plugin via `[[plugins.grants]]`.
- **Path scopes** match on path components, not bytes —
  `fs.read:/etc` allows `/etc/passwd` but **not** `/etcd-data/x`.
- **Path traversal** (`..`) is rejected at parse time *and* on every
  per-call query.
- **Denials trap** for write operations; `state-get` returns `None`
  to keep cardinality side-channels closed.
- **Audit log** emits one structured `tracing::warn!` per first
  denial under target `narwhal::plugin::audit`.

## Resource limits

Each plugin gets its own `wasmtime::Store`. The defaults are:

| Knob                       | Default      | Source                                 |
| -------------------------- | ------------ | -------------------------------------- |
| Memory                     | 64 MiB       | `RuntimeConfig::memory_limit`          |
| Fuel per export call       | 100 M ops    | `RuntimeConfig::fuel_per_call`         |
| KV byte budget             | 256 KiB      | `RuntimeConfig::kv_budget`             |

A misbehaving plugin that allocates past the memory ceiling is killed by
wasmtime; a plugin that overspends fuel traps cleanly and the host logs the
failure without disturbing other plugins.

## See also

- `docs/dev/t1-t5-a-wasm-runtime.md` — Tier-2 contract notes (capability
  boundary, future MCP-tool track, breakage policy).
- `docs/dev/t1-t5-b-sandbox.md` — host-side implementation notes for the
  capability + enforcement model.
- `docs/plugins/security.md` — threat model + operator playbook.
- `docs/plugins/wasm.md` — plugin-author SDK walkthrough.
- `wit/world.wit` — the canonical interface contract; this file is the
  source of truth for plugin authors and host implementors.

[nw]: https://github.com/Nonanti/narwhal
[lua]: ../narwhal-plugin-lua
[cm]: https://component-model.bytecodealliance.org/
