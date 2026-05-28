//! Host-side glue for the WASM plugin runtime.
//!
//! Three concerns live here:
//!
//! 1. The [`HostState`] struct stored in every plugin's `wasmtime::Store`,
//! along with the [`bindings::narwhal::plugin::host::Host`] impl that
//! bridges WASM calls to host data.
//! 2. The [`CommandBus`] / [`LogSink`] traits the host injects so the
//! crate stays free of a hard `narwhal-app` dependency.
//! 3. A wasmtime [`ResourceLimiter`] wrapper that caps a misbehaving
//! plugin's memory growth (the `StoreLimits` themselves live in
//! [`crate::limits::memory`]).
//!
//! ## Capability enforcement
//!
//! Each host fn entry point translates its inputs to an
//! [`crate::sandbox::Operation`], asks the per-plugin
//! [`crate::sandbox::Enforcer`] for a [`crate::sandbox::Decision`],
//! and converts denial into:
//!
//! * **hard trap** for `host.cmd` and `host.state-set` — the call
//! never observes its effect, the plugin sees a
//! [`wasmtime::Trap`]. returned a polite
//! `command-result{ok:false}` and silently dropped state-set;
//! raises both to traps so a hostile plugin cannot ignore
//! the denial.
//! * **`None` return** for `host.state-get` — read traffic is
//! information-disclosure adjacent; the safer default is to look
//! like the key never existed. The audit log still records the
//! denial.
//!
//! Every denial emits a structured [`crate::sandbox::AuditEvent`]
//! through the runtime's configured [`crate::sandbox::AuditSink`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;
use wasmtime::component::HasData;
use wasmtime::{ResourceLimiter, StoreLimits};

use std::collections::HashMap;
use std::sync::Mutex;

use crate::bindings::narwhal::plugin::host::Host;
use crate::bindings::narwhal::plugin::types::CommandResult;
use crate::capability::CapabilitySet;
use crate::limits::{KvAccount, KvOutcome};
use crate::sandbox::{Decision, Enforcer, Operation, StandardEnforcer};

/// Zero-sized marker that lets the wasmtime linker's [`HasData`]
/// trait point at [`HostState`] without leaking the choice into
/// callers. The bindgen-generated `add_to_linker` is parametric in
/// `D: HasData` to support split-store designs; we use the simplest
/// shape: data lives directly on the store.
pub(crate) struct HostMarker;

impl HasData for HostMarker {
    type Data<'a> = &'a mut HostState;
}

/// Where plugin log lines go. The binary wires the production sink to
/// [`tracing`] via [`TracingLogSink`]; tests use [`RecordingLogSink`]
/// to assert on what a plugin emitted.
pub trait LogSink: Send + Sync {
    fn emit(&self, line: LogLine);
}

/// One log record produced by a plugin's `host.log` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LogLine {
    pub plugin: String,
    pub level: String,
    pub message: String,
}

impl LogLine {
    /// Convenience constructor for embedders / tests.
    pub fn new(
        plugin: impl Into<String>,
        level: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            level: level.into(),
            message: message.into(),
        }
    }
}

/// Trait the host implements to let plugins dispatch `:` commands.
///
/// Kept abstract so this crate doesn't pull in `narwhal-app`. The
/// binary's `AppCore` provides the concrete implementation; this crate
/// ships a [`NoopCommandBus`] for tests and embed-only consumers.
#[async_trait]
pub trait CommandBus: Send + Sync {
    /// Run command `name` with `args`. Returns the user-facing status
    /// line on success or an error message on failure.
    async fn dispatch(&self, name: &str, args: &[String]) -> Result<String, String>;
}

/// Default command bus: every dispatch fails politely with a message
/// describing the missing wiring. Lets the crate function as a library
/// without forcing every embedder to provide a bus implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCommandBus;

#[async_trait]
impl CommandBus for NoopCommandBus {
    async fn dispatch(&self, _name: &str, _args: &[String]) -> Result<String, String> {
        Err("no command bus is wired (run inside narwhal-app to enable host.cmd)".into())
    }
}

/// Production log sink: forwards every line to `tracing` at the
/// appropriate level, tagged with the plugin name.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingLogSink;

impl LogSink for TracingLogSink {
    fn emit(&self, line: LogLine) {
        match line.level.as_str() {
            "trace" => tracing::trace!(plugin = %line.plugin, "{}", line.message),
            "debug" => tracing::debug!(plugin = %line.plugin, "{}", line.message),
            "warn" => tracing::warn!(plugin = %line.plugin, "{}", line.message),
            "error" => tracing::error!(plugin = %line.plugin, "{}", line.message),
            // info + anything unknown collapses to info — matches the
            // WIT-level contract in `wit/world.wit`.
            _ => tracing::info!(plugin = %line.plugin, "{}", line.message),
        }
    }
}

/// Test sink: stores log lines in a shared `Vec` so test code can
/// assert on them.
#[derive(Debug, Clone, Default)]
pub struct RecordingLogSink {
    inner: Arc<Mutex<Vec<LogLine>>>,
}

impl RecordingLogSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the captured log lines. Returns a clone so the caller
    /// can drop the lock immediately.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Drain captured log lines, leaving the sink empty afterwards.
    pub fn drain(&self) -> Vec<LogLine> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

impl LogSink for RecordingLogSink {
    fn emit(&self, line: LogLine) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(line);
        }
    }
}

/// State each plugin's `Store` carries.
///
/// Wraps:
///
/// * `name` — used as the plugin namespace in audit logs and the
/// `LogLine::plugin` field.
/// * `enforcer` — the per-call policy guard. The runtime
/// constructs a [`StandardEnforcer`] from the manifest's
/// intersected capability set; tests can swap in their own
/// [`Enforcer`] impl via [`HostState::with_enforcer`].
/// * `kv` — in-memory KV map serialised behind a `std::sync::Mutex`
/// so concurrent host calls can't corrupt it.
/// * `kv_account` — byte budget tracker, see [`crate::limits::KvAccount`].
/// * `log_sink` / `command_bus` — runtime-injected service delegates.
/// * `limits` — wasmtime memory cap.
pub struct HostState {
    pub(crate) name: String,
    pub(crate) enforcer: Arc<dyn Enforcer>,
    pub(crate) kv: AsyncMutex<HashMap<String, Vec<u8>>>,
    pub(crate) kv_account: AsyncMutex<KvAccount>,
    pub(crate) log_sink: Arc<dyn LogSink>,
    pub(crate) command_bus: Arc<dyn CommandBus>,
    pub(crate) limits: StoreLimits,
}

impl HostState {
    pub(crate) fn new(
        name: String,
        enforcer: Arc<dyn Enforcer>,
        kv_budget: usize,
        log_sink: Arc<dyn LogSink>,
        command_bus: Arc<dyn CommandBus>,
        limits: StoreLimits,
    ) -> Self {
        Self {
            name,
            enforcer,
            kv: AsyncMutex::new(HashMap::new()),
            kv_account: AsyncMutex::new(KvAccount::new(kv_budget)),
            log_sink,
            command_bus,
            limits,
        }
    }

    /// Replace the per-plugin enforcer. Used by tests that need a
    /// hand-rolled [`Enforcer`] (typically one that records every
    /// `check` call or always denies).
    #[must_use]
    pub fn with_enforcer(mut self, enforcer: Arc<dyn Enforcer>) -> Self {
        self.enforcer = enforcer;
        self
    }

    /// Borrow the active enforcer. Useful in tests for cache-size
    /// assertions.
    pub fn enforcer(&self) -> Arc<dyn Enforcer> {
        Arc::clone(&self.enforcer)
    }

    /// Hand the wasmtime memory/table limiter back to the store. The
    /// store's `limiter` closure expects `&mut dyn ResourceLimiter`
    /// re-fetched on every allocation event.
    pub(crate) fn limiter(&mut self) -> &mut dyn ResourceLimiter {
        &mut self.limits
    }

    /// Snapshot the current KV byte usage. Used by integration tests
    /// to assert budget enforcement.
    pub async fn kv_used(&self) -> usize {
        self.kv_account.lock().await.used()
    }

    /// Borrow a KV value by key (read-only).
    pub async fn kv_get_snapshot(&self, key: &str) -> Option<Vec<u8>> {
        self.kv.lock().await.get(key).cloned()
    }
}

// ---------------------------------------------------------------------------
// Host trait impl — bridges the WIT-declared functions onto HostState.
// `bindgen!` with `imports: { default: async }` generates native
// `async fn` signatures, so we deliberately do *not* use `#[async_trait]`
// here (it would conflict with the bindgen-generated lifetime shape).
//
// Every entry point routes through `self.enforcer.check(...)`; denial
// becomes a wasmtime trap or a polite read-side absence depending on
// the variant. See the module-level docstring for the full matrix.
// ---------------------------------------------------------------------------

impl Host for HostState {
    async fn cmd(&mut self, name: String, args: Vec<String>) -> wasmtime::Result<CommandResult> {
        let op = Operation::CmdInvoke { name: name.clone() };
        match self.enforcer.check(&self.name, &op) {
            Decision::Allow => Ok(match self.command_bus.dispatch(&name, &args).await {
                Ok(message) => CommandResult { ok: true, message },
                Err(message) => CommandResult { ok: false, message },
            }),
            Decision::Deny {
                kind,
                reason,
                audit_id,
            } => {
                // Hard trap — the plugin sees a wasmtime error,
                // not a polite ok:false. Operators correlate the
                // trap to the audit log via the audit id.
                Err(wasmtime::Error::msg(format!(
                    "plugin '{}' denied {kind}: {reason} ({audit_id})",
                    self.name
                )))
            }
        }
    }

    async fn log(&mut self, level: String, message: String) -> wasmtime::Result<()> {
        // Logging is unconditionally allowed — it is the host's
        // observability channel and gating it would hide the audit
        // trail itself. The audit log still emits its own events
        // through `tracing`, separate from plugin-emitted lines.
        self.log_sink.emit(LogLine {
            plugin: self.name.clone(),
            level,
            message,
        });
        Ok(())
    }

    async fn state_get(&mut self, key: String) -> wasmtime::Result<Option<Vec<u8>>> {
        if !matches!(
            self.enforcer.check(&self.name, &Operation::StateAccess),
            Decision::Allow
        ) {
            // Refusing reads with `None` keeps cardinality side
            // channels closed: a denied plugin can't probe for
            // key existence by varying the trap behaviour.
            return Ok(None);
        }
        Ok(self.kv.lock().await.get(&key).cloned())
    }

    async fn state_set(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<()> {
        match self.enforcer.check(&self.name, &Operation::StateAccess) {
            Decision::Allow => {}
            Decision::Deny {
                kind,
                reason,
                audit_id,
            } => {
                return Err(wasmtime::Error::msg(format!(
                    "plugin '{}' denied {kind}: {reason} ({audit_id})",
                    self.name
                )));
            }
        }
        let new_len = value.len();
        // Acquire the KV lock first, then the account lock, then
        // hold *both* through the project/commit/insert sequence.
        // Two concurrent `state_set` calls would otherwise race:
        // both could observe the same `prev_len`, both could pass
        // the budget check, and the later commit would clobber the
        // earlier one with stale accounting. The pair-acquire order
        // (kv → kv_account, never reversed elsewhere) prevents
        // deadlock.
        let mut kv = self.kv.lock().await;
        let mut account = self.kv_account.lock().await;
        let prev_len = kv.get(&key).map_or(0, Vec::len);
        let outcome = account.project(prev_len, new_len);
        match outcome {
            KvOutcome::Accepted { .. } => {
                account.commit(outcome);
                kv.insert(key, value);
                Ok(())
            }
            KvOutcome::Rejected { projected, budget } => {
                tracing::warn!(
                    plugin = %self.name,
                    projected, budget,
                    "host.state-set refused: KV budget exhausted"
                );
                // KV-budget overruns trap so the plugin can adapt
                // (catch it and write smaller payloads) rather than
                // silently lose data. Matches the boundary
                // table promise of "trap upgrade".
                Err(wasmtime::Error::msg(format!(
                    "plugin '{}' KV budget exhausted: {projected}/{budget} bytes",
                    self.name
                )))
            }
        }
    }
}

/// Construct a fresh [`StandardEnforcer`] wrapped in `Arc<dyn>` so
/// the runtime can hand it straight to [`HostState`]. Kept out of
/// the `HostState::new` signature so embedders can swap in a custom
/// [`Enforcer`] without going through the constructor at all.
#[must_use]
pub fn standard_enforcer(
    effective: CapabilitySet,
    audit: Arc<dyn crate::sandbox::AuditSink>,
    broad_cmd: bool,
) -> Arc<dyn Enforcer> {
    Arc::new(StandardEnforcer::new(effective, audit, broad_cmd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, CapabilitySet};
    use crate::sandbox::{NoopAuditSink, RecordingAuditSink};

    fn enforcer_with_caps(caps: Vec<Capability>) -> Arc<dyn Enforcer> {
        standard_enforcer(
            CapabilitySet::from_caps(caps),
            Arc::new(NoopAuditSink) as Arc<dyn crate::sandbox::AuditSink>,
            false,
        )
    }

    fn make_state(caps: Vec<Capability>, budget: usize) -> HostState {
        HostState::new(
            "test".into(),
            enforcer_with_caps(caps),
            budget,
            Arc::new(RecordingLogSink::new()),
            Arc::new(NoopCommandBus),
            StoreLimits::default(),
        )
    }

    #[tokio::test]
    async fn log_routes_through_sink_unconditionally() {
        let sink = Arc::new(RecordingLogSink::new());
        let mut state = HostState::new(
            "echo".into(),
            enforcer_with_caps(vec![]),
            1024,
            sink.clone(),
            Arc::new(NoopCommandBus),
            StoreLimits::default(),
        );
        state.log("info".into(), "hello".into()).await.unwrap();
        let lines = sink.snapshot();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].plugin, "echo");
    }

    #[tokio::test]
    async fn state_round_trips_when_capability_granted() {
        let mut state = make_state(vec![Capability::State], 1024);
        state.state_set("k".into(), b"hi".to_vec()).await.unwrap();
        let got = state.state_get("k".into()).await.unwrap();
        assert_eq!(got.as_deref(), Some(b"hi".as_slice()));
        assert_eq!(state.kv_used().await, 2);
    }

    #[tokio::test]
    async fn state_get_returns_none_without_capability() {
        let mut state = make_state(vec![], 1024);
        // state-set without capability traps; verify that path too.
        let err = state
            .state_set("k".into(), b"hi".to_vec())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied"));
        let got = state.state_get("k".into()).await.unwrap();
        assert!(got.is_none());
        assert_eq!(state.kv_used().await, 0);
    }

    #[tokio::test]
    async fn state_set_overrun_traps() {
        let mut state = make_state(vec![Capability::State], 4);
        state.state_set("k".into(), b"hi".to_vec()).await.unwrap();
        // 2 bytes used.
        let err = state
            .state_set("k2".into(), b"world".to_vec())
            .await
            .unwrap_err();
        // would push to 7 > 4 → trap.
        assert!(err.to_string().contains("KV budget exhausted"));
        assert_eq!(state.kv_used().await, 2);
        assert!(state.kv_get_snapshot("k2").await.is_none());
    }

    #[tokio::test]
    async fn cmd_without_capability_traps() {
        let mut state = make_state(vec![], 1024);
        let err = state.cmd("anything".into(), vec![]).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("denied"));
        assert!(msg.contains("cmd.invoke"));
    }

    #[tokio::test]
    async fn cmd_with_explicit_grant_reaches_bus() {
        let mut state = make_state(vec![Capability::CmdInvoke("run".into())], 1024);
        let res = state.cmd("run".into(), vec![]).await.unwrap();
        // NoopCommandBus returns Err(...) with explanatory text;
        // we land in the ok:false arm of `cmd`.
        assert!(!res.ok);
        assert!(res.message.contains("no command bus"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn state_set_concurrent_writes_stay_within_budget() {
        // Race-regression: two concurrent state_set calls with the
        // shared lock ordering must not double-account.
        let state = Arc::new(tokio::sync::Mutex::new(make_state(
            vec![Capability::State],
            10,
        )));

        let mut handles = Vec::new();
        for i in 0..50_u8 {
            let state = Arc::clone(&state);
            handles.push(tokio::spawn(async move {
                let mut guard = state.lock().await;
                // Single byte per write — the budget caps at 10
                // distinct keys.
                let _ = guard.state_set(format!("k{i}"), vec![0u8]).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let guard = state.lock().await;
        // Whatever the interleaving, total bytes equals number of
        // committed keys; never exceeds the budget.
        assert!(
            guard.kv_used().await <= 10,
            "used={} should not exceed budget 10",
            guard.kv_used().await
        );
    }

    #[tokio::test]
    async fn audit_log_records_denials() {
        let audit = Arc::new(RecordingAuditSink::new());
        let enforcer = standard_enforcer(
            CapabilitySet::new(),
            audit.clone() as Arc<dyn crate::sandbox::AuditSink>,
            false,
        );
        let mut state = HostState::new(
            "test".into(),
            enforcer,
            1024,
            Arc::new(RecordingLogSink::new()),
            Arc::new(NoopCommandBus),
            StoreLimits::default(),
        );
        let _ = state.cmd("x".into(), vec![]).await;
        let _ = state.state_set("k".into(), b"v".to_vec()).await;
        let snap = audit.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.iter().any(|e| e.operation == "cmd.invoke:x"));
        assert!(snap.iter().any(|e| e.operation == "state"));
    }
}
