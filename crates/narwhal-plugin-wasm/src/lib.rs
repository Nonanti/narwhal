//! WebAssembly component-model plugin runtime for narwhal.
//!
//! This crate is the second plugin track that ships with narwhal 2.0 —
//! a counterpart to the existing [`narwhal-plugin-lua`][lua] runtime.
//! Plugins are `.wasm` *components* (not core wasm modules) produced
//! by any toolchain that targets [the component model][cm]; the
//! shipped example uses Rust + `wit-bindgen`, and the SDK doc covers a
//! TypeScript track via `componentize-js`.
//!
//! ## Architecture at a glance
//!
//! ```text
//! config + plugin.toml ─┐
//! │
//! Manifest::load        ┌── one Engine per host
//! │               ▼
//! Runtime::load ──► wasmtime::Engine
//! │               │
//! │               ▼
//! │       wasmtime::component::Component
//! │               │
//! ▼               ▼
//! WasmPlugin (impl Plugin) Store<HostState>
//! │               │
//! ▼               ▼
//! PluginRegistry::register   host functions (cmd/log/state)
//! ```
//!
//! * One [`Runtime`] per process. It owns the shared
//! [`wasmtime::Engine`] and the global host policy (capability +
//! resource limits sourced from `Settings::plugins.wasm`).
//! * One [`WasmPlugin`] per loaded component. Each owns its own
//! [`wasmtime::Store`], its own KV namespace, its own fuel budget
//! and (eventually) its own filesystem capability set.
//! * The crate implements [`narwhal_plugin::Plugin`] for
//! [`WasmPlugin`] so the surrounding `PluginRegistry` machinery
//! does not care which runtime produced the trait object.
//!
//! ## v0.1 scope and what comes later
//!
//! What lands in 2.0:
//!
//! * Component loading + manifest validation + capability matching.
//! * Memory + fuel limits enforced (default 64 MiB / 100 M fuel per
//! event invocation, configurable per plugin via the manifest).
//! * Host functions for logging and per-plugin KV. `cmd` is wired
//! through an injectable [`CommandBus`] trait — the binary
//! provides the implementation; this crate ships a no-op stub so
//! non-app consumers still compile.
//! * Lifecycle hook: every loaded plugin receives `on-event` for the
//! events listed in `wit/world.wit`. Errors are isolated; one bad
//! plugin can't crash the host.
//!
//! What is *deliberately* deferred to follow-up tasks:
//!
//! * Filesystem / network / env capability enforcement.
//! The manifest can declare these; the runtime today only stores
//! the set on the [`HostState`] for to read.
//! * MCP-tool plugin track.
//! * Hot reload — restart is required to load or unload a plugin.
//! * Persistent KV; today's store is an in-memory `HashMap`.
//!
//! See `docs/dev/wasm-runtime.md` for the full Tier 2
//! contract and roadmap deltas.
//!
//! [lua]: ../narwhal_plugin_lua/index.html
//! [cm]: https://component-model.bytecodealliance.org/

#![forbid(unsafe_code)]

pub mod capability;
mod error;
mod host;
mod instance;
pub mod limits;
mod manifest;
mod runtime;
pub mod sandbox;

pub use capability::{
    Capability, CapabilityKind, CapabilityParseError, CapabilitySet, EnvVar, Grants, HostPort,
    PathScope,
};
pub use error::{WasmError, WasmResult};
pub use host::{
    CommandBus, HostState, LogLine, LogSink, NoopCommandBus, RecordingLogSink, TracingLogSink,
    standard_enforcer,
};
pub use instance::WasmPlugin;
pub use limits::{FuelMeter, KvAccount, KvOutcome};
pub use manifest::{HOST_API_MAJOR, Manifest};
pub use runtime::{
    DEFAULT_FUEL_BUDGET, DEFAULT_KV_BUDGET, DEFAULT_MEMORY_LIMIT, Runtime, RuntimeConfig,
};
pub use sandbox::{
    AUDIT_TARGET, AuditEvent, AuditId, AuditSink, Decision, DecisionCache, Enforcer, NoopAuditSink,
    Operation, RecordingAuditSink, StandardEnforcer, TracingAuditSink,
};

// ---------------------------------------------------------------------------
// Generated bindings.
//
// Re-exposed as `narwhal_plugin_wasm::bindings` so external plumbing (the
// future MCP-tool track, integration tests) can name the canonical types
// without re-running `bindgen!`. The crate's *public* surface stays the
// hand-written types above; `bindings` is documented as
// "implementation-defined, semver-fragile". It widens or narrows in lock
// step with `wit/world.wit`.
//
// Lint allows mirror the workspace style we use for every other generated
// shim (e.g. the diagram-renderer auto-tables) — keeping pedantic+nursery
// on `cargo clippy` cleanly off the macro-generated code.
// ---------------------------------------------------------------------------
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_docs,
    unreachable_pub,
    dead_code,
    unused_imports,
    unused_qualifications,
    elided_lifetimes_in_paths,
    single_use_lifetimes
)]
pub mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/world.wit",
        world: "narwhal-plugin",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}
