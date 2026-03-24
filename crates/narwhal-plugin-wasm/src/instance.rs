//! [`WasmPlugin`] — one loaded `.wasm` component, plus the
//! [`narwhal_plugin::Plugin`] trait impl that lets the surrounding
//! `PluginRegistry` treat it the same as a Lua plugin.
//!
//! Each [`WasmPlugin`] owns:
//!
//! * the parsed [`Manifest`] (kept so `name()`, `commands()`, and
//!   logging stay cheap),
//! * a [`tokio::sync::Mutex`] guarding the
//!   [`wasmtime::Store`] + [`crate::bindings::NarwhalPlugin`] pair
//!   (wasmtime stores are not `Sync`; the mutex serialises every
//!   call into the wasm module),
//! * a back-reference to the [`Runtime`] so each call can re-fuel
//!   the store before dispatch.
//!
//! Wasmtime's async exports require a multi-threaded tokio runtime
//! (so the host-side `await`s can yield without blocking). The
//! workspace runtime is multi-thread by default; tests in this crate
//! annotate themselves with `#[tokio::test(flavor = "multi_thread")]`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use wasmtime::Store;

use narwhal_plugin::{
    CommandContext, CommandDescriptor, CommandOutcome, Plugin, PluginError, PluginEvent,
    PluginResult, QueryResult,
};

use crate::bindings;
use crate::bindings::narwhal::plugin::types::{
    BufferSnapshot, CommandInput, CommandOutcome as WitOutcome, Event as WitEvent, QuerySummary,
};
use crate::host::HostState;
use crate::manifest::Manifest;
use crate::runtime::{Runtime, refuel};

/// One loaded WASM plugin.
///
/// Cloning is cheap — the underlying store + bindings are shared via
/// `Arc`, the [`Runtime`] is `Clone`, and the [`Manifest`] is small.
/// The host is expected to hold the plugin behind
/// `Arc<dyn Plugin>` (the [`narwhal_plugin::PluginRegistry`] shape)
/// after registration, so the `Clone` impl is provided mostly for
/// test code that needs to drive multiple call paths concurrently.
#[derive(Clone)]
pub struct WasmPlugin {
    manifest: Arc<Manifest>,
    inner: Arc<PluginInner>,
}

struct PluginInner {
    /// Serialises every wasm export call. The store + bindings must
    /// stay together: bindings hold typed handles into the store.
    store: Mutex<Store<HostState>>,
    bindings: bindings::NarwhalPlugin,
    runtime: Runtime,
}

impl WasmPlugin {
    pub(crate) fn new(
        manifest: Manifest,
        store: Store<HostState>,
        bindings: bindings::NarwhalPlugin,
        runtime: Runtime,
    ) -> Self {
        Self {
            manifest: Arc::new(manifest),
            inner: Arc::new(PluginInner {
                store: Mutex::new(store),
                bindings,
                runtime,
            }),
        }
    }

    /// Borrow the validated manifest. Used by integration code that
    /// wants to surface plugin metadata (e.g. the `:plugins` palette).
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Refuel the store and call the plugin's `on-event` export. The
    /// `Plugin::on_event` trait method forwards to this; broken out
    /// so tests can drive events directly without round-tripping
    /// through the trait object.
    pub async fn deliver_event(&self, event: &PluginEvent) -> PluginResult<()> {
        let wit_event = translate_event(event);
        let mut guard = self.inner.store.lock().await;
        refuel(&mut guard, self.inner.runtime.config().fuel_per_call).map_err(PluginError::from)?;
        let res = self
            .inner
            .bindings
            .narwhal_plugin_plugin()
            .call_on_event(&mut *guard, &wit_event)
            .await
            .map_err(|e| {
                PluginError::Runtime(format!("plugin '{}' on-event: {e}", self.manifest.name))
            })?;
        res.map_err(PluginError::Handler)
    }
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn commands(&self) -> Vec<CommandDescriptor> {
        self.manifest.commands.clone()
    }

    async fn dispatch(&self, name: &str, ctx: CommandContext) -> PluginResult<CommandOutcome> {
        // Refuse names the manifest didn't declare. The host already
        // routes via `plugin_for(name)`, but this defence-in-depth
        // arm guards against the `:` parser ever handing us an
        // unrelated command (e.g. via a future broadcast feature).
        if !self.manifest.commands.iter().any(|c| c.name == name) {
            return Err(PluginError::Unknown(name.to_owned()));
        }
        let input = CommandInput {
            name: name.to_owned(),
            argument: ctx.argument,
            editor_text: ctx.editor_text,
        };
        let mut guard = self.inner.store.lock().await;
        refuel(&mut guard, self.inner.runtime.config().fuel_per_call).map_err(PluginError::from)?;
        let outcome = self
            .inner
            .bindings
            .narwhal_plugin_plugin()
            .call_handle_command(&mut *guard, &input)
            .await
            .map_err(|e| {
                PluginError::Runtime(format!(
                    "plugin '{}' handle-command: {e}",
                    self.manifest.name
                ))
            })?;
        match outcome {
            Ok(o) => Ok(translate_outcome(o)),
            Err(msg) => Err(PluginError::Handler(msg)),
        }
    }

    /// No-op for v0.1.
    ///
    /// The WIT contract does not expose a `transform_result` export
    /// (richly modelling [`QueryResult`] in component types would
    /// double the surface). When the brief calls for transforms in
    /// a follow-up minor, a dedicated `transform-result` interface
    /// will be added — the trait shape stays the same.
    async fn transform_result(&self, _result: &mut QueryResult) -> PluginResult<()> {
        Ok(())
    }

    async fn on_event(&self, event: &PluginEvent) -> PluginResult<()> {
        self.deliver_event(event).await
    }
}

impl std::fmt::Debug for WasmPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmPlugin")
            .field("name", &self.manifest.name)
            .field("version", &self.manifest.version)
            .field("commands", &self.manifest.commands.len())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Translation helpers between the host's [`narwhal_plugin`] types and the
// WIT-generated bindings.
// ---------------------------------------------------------------------------

fn translate_event(event: &PluginEvent) -> WitEvent {
    match event {
        PluginEvent::ConnectionOpened { name } => WitEvent::ConnectionOpened(name.clone()),
        PluginEvent::ConnectionClosed { name } => WitEvent::ConnectionClosed(name.clone()),
        PluginEvent::QueryStarted { sql } => WitEvent::QueryStarted(sql.clone()),
        PluginEvent::QueryFinished {
            sql,
            rows,
            elapsed_ms,
            ok,
        } => WitEvent::QueryFinished(QuerySummary {
            sql: sql.clone(),
            rows: *rows,
            elapsed_ms: *elapsed_ms,
            ok: *ok,
        }),
        PluginEvent::EditorBufferChanged {
            text,
            cursor_line,
            cursor_col,
        } => WitEvent::EditorBufferChanged(BufferSnapshot {
            text: text.clone(),
            cursor_line: *cursor_line,
            cursor_col: *cursor_col,
        }),
        // Forward-compat arm for variants added to `PluginEvent` in a
        // later host version — refuse to translate so the plugin sees
        // a clear error rather than the wrong variant. Matches the
        // workspace `non_exhaustive` enum convention.
        #[allow(unreachable_patterns)]
        _ => {
            debug_assert!(false, "missing PluginEvent translate arm; open an issue");
            tracing::error!("PluginEvent translate hit the unknown arm; open an issue");
            // Use ConnectionOpened with a sentinel name so the plugin
            // can at least see *something* and the host logs the
            // anomaly. Defensive; a future host bump must add the arm.
            WitEvent::ConnectionOpened(String::from("<unknown-event>"))
        }
    }
}

fn translate_outcome(o: WitOutcome) -> CommandOutcome {
    match o {
        WitOutcome::Status(message) => CommandOutcome::Status { message },
        WitOutcome::InsertSql(p) => CommandOutcome::InsertSql {
            sql: p.sql,
            append: p.append,
        },
        WitOutcome::Silent => CommandOutcome::Silent,
        // Forward-compat: future WIT variants get logged + collapse
        // to Silent. WIT enums are closed on the wire, so this arm
        // is reachable only after a host upgrade adds a variant the
        // translation table forgot.
        #[allow(unreachable_patterns)]
        _ => {
            debug_assert!(false, "missing CommandOutcome translate arm; open an issue");
            tracing::error!("CommandOutcome translate hit the unknown arm; open an issue");
            CommandOutcome::Silent
        }
    }
}
