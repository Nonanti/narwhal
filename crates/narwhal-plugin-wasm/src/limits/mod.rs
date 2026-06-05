//! Resource-limit primitives for the WASM plugin runtime.
//!
//! Three independent concerns:
//!
//! * **Memory** — [`memory::build_store_limits`] returns the
//!   [`wasmtime::StoreLimits`] the engine consults on every linear
//!   memory growth event. Calibrated from
//!   [`crate::RuntimeConfig::memory_limit`].
//! * **Fuel** — [`fuel::FuelMeter`] tops the store up before each
//!   export call and records the consumed amount when the call
//!   returns. Exposes a cheap accessor for tests.
//! * **KV byte budget** — [`kv::KvAccount`] tracks the per-plugin
//!   in-memory KV store size against
//!   [`crate::RuntimeConfig::kv_budget`]. The host-fn entry point
//!   for `state-set` consults the account to decide whether to
//!   accept the write or trap.
//!
//! Splitting them into a dedicated module keeps `host.rs` focused
//! on the WIT-binding glue.

pub mod fuel;
pub mod kv;
pub mod memory;

pub use fuel::FuelMeter;
pub use kv::{KvAccount, KvOutcome};
pub use memory::build_store_limits;
