//! Process-wide WASM plugin runtime.
//!
//! Owns the shared [`wasmtime::Engine`] and the host policy used to
//! validate every plugin's manifest. The expected lifecycle is:
//!
//! 1. Build a [`Runtime`] at startup.
//! 2. Walk the plugin directory; call [`Runtime::load`] for each
//! `plugin.toml`.
//! 3. Register the returned [`WasmPlugin`] objects with the
//! surrounding `PluginRegistry`.

use std::path::Path;
use std::sync::Arc;

use narwhal_config::WasmPluginSettings;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

use crate::bindings;
use crate::capability::{Capability, CapabilitySet, EnvVar, Grants, HostPort, PathScope};
use crate::error::{WasmError, WasmResult};
use crate::host::{
    CommandBus, HostMarker, HostState, LogSink, NoopCommandBus, TracingLogSink, standard_enforcer,
};
use crate::instance::WasmPlugin;
use crate::limits::{FuelMeter, build_store_limits};
use crate::manifest::Manifest;
use crate::sandbox::{AuditSink, TracingAuditSink};

/// Default per-plugin memory ceiling (64 MiB). The wasmtime
/// `StoreLimits` machinery enforces this on every linear-memory
/// growth event.
pub const DEFAULT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Default per-event-invocation fuel budget (~100 M instructions).
/// Calibrated against the wasmtime defaults rather than a wall-clock
/// number; consumers should not assume a tight time bound.
pub const DEFAULT_FUEL_BUDGET: u64 = 100_000_000;

/// Default per-plugin KV byte budget (256 KiB). Small on purpose —
/// the KV is meant for tiny configuration data, not bulk payloads.
pub const DEFAULT_KV_BUDGET: usize = 256 * 1024;

/// Tunables applied to every plugin loaded through this runtime.
///
/// Constructed via [`RuntimeConfig::default`] + field assignment to
/// keep the `#[non_exhaustive]` policy: new knobs may be added without
/// breaking call sites that already use `RuntimeConfig::default()`.
///
/// `grants` is the *fine* policy — per-path, per-host, per-var
/// allow-lists. `settings_policy` is the *coarse* policy parsed from
/// the user's `narwhal.toml` (`[plugins.wasm]`). Both are honoured:
/// the manifest's declared set must satisfy both gates before the
/// plugin is allowed to load.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RuntimeConfig {
    /// Hard memory ceiling per plugin Store, in bytes.
    pub memory_limit: usize,
    /// Fuel topped up before every export call.
    pub fuel_per_call: u64,
    /// Per-plugin KV byte budget.
    pub kv_budget: usize,
    /// User-settings-derived coarse policy (FS/net/env bool flags).
    pub settings_policy: WasmPluginSettings,
    /// Fine grants — defaults to deny-all except `state`.
    pub grants: Grants,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit: DEFAULT_MEMORY_LIMIT,
            fuel_per_call: DEFAULT_FUEL_BUDGET,
            kv_budget: DEFAULT_KV_BUDGET,
            settings_policy: WasmPluginSettings::default(),
            grants: Grants::deny_all(),
        }
    }
}

impl RuntimeConfig {
    /// Build a [`Grants`] from the coarse settings flags. When the
    /// embedder supplies fine [`Grants`] explicitly via
    /// [`RuntimeConfig::grants`] those take precedence; this helper
    /// is for embedders that only have the coarse settings to work
    /// with.
    ///
    /// The mapping mirrors's policy match:
    ///
    /// | settings flag      | granted capability                          |
    /// | ------------------ | ------------------------------------------- |
    /// | `allow_fs_read`    | `FsRead("/")`  — root, widest scope         |
    /// | `allow_fs_write`   | `FsWrite("/")` — root, widest scope         |
    /// | `allow_net`        | `NetConnect("*")` — wildcard host           |
    /// | `allow_env`        | `EnvRead("*")` — wildcard var               |
    ///
    /// Always grants [`Capability::State`] (per-plugin KV, no
    /// security boundary).
    #[must_use]
    pub fn grants_from_settings(settings: &WasmPluginSettings) -> Grants {
        // `State` and `Cmd` are host-internal capabilities with no
        // FS / net / env boundary;'s `check_allowed`
        // unconditionally granted them. Preserve that semantic in
        // the settings layer so existing manifests carrying bare
        // `state` / `cmd` keep loading after the upgrade.
        let mut caps = vec![Capability::State, Capability::Cmd];
        if settings.allow_fs_read {
            caps.push(Capability::FsRead(PathScope::root()));
        }
        if settings.allow_fs_write {
            caps.push(Capability::FsWrite(PathScope::root()));
        }
        if settings.allow_net {
            caps.push(Capability::NetConnect(HostPort::wildcard()));
        }
        if settings.allow_env {
            caps.push(Capability::EnvRead(EnvVar::wildcard()));
        }
        Grants {
            inner: CapabilitySet::from_caps(caps),
            allow_state_default: true,
            // broad_cmd stays false; the bare `Cmd` token in `inner`
            // is what covers legacy bare-cmd manifests. Setting
            // `broad_cmd` here would also let *every* future
            // `cmd.invoke:*` token coast through without an explicit
            // grant — too lax.
            broad_cmd: false,
        }
    }
}

/// Host-side WASM plugin runtime.
#[derive(Clone)]
pub struct Runtime {
    engine: Engine,
    linker: Arc<Linker<HostState>>,
    config: RuntimeConfig,
    log_sink: Arc<dyn LogSink>,
    command_bus: Arc<dyn CommandBus>,
    audit_sink: Arc<dyn AuditSink>,
}

impl Runtime {
    /// Build a runtime with [`RuntimeConfig::default`] and a tracing
    /// log + audit sink.
    pub fn new() -> WasmResult<Self> {
        Self::with_config(RuntimeConfig::default())
    }

    /// Build a runtime with a custom config.
    pub fn with_config(config: RuntimeConfig) -> WasmResult<Self> {
        Self::build(
            config,
            Arc::new(TracingLogSink) as Arc<dyn LogSink>,
            Arc::new(NoopCommandBus) as Arc<dyn CommandBus>,
            Arc::new(TracingAuditSink) as Arc<dyn AuditSink>,
        )
    }

    /// Replace the active log sink. Returns the runtime back so the
    /// call chains cleanly off [`Runtime::new`].
    #[must_use]
    pub fn with_log_sink(mut self, sink: Arc<dyn LogSink>) -> Self {
        self.log_sink = sink;
        self
    }

    /// Inject a [`CommandBus`] so plugins' `host.cmd` calls reach the
    /// surrounding application. The default
    /// [`NoopCommandBus`] returns an explanatory error to every
    /// dispatch.
    #[must_use]
    pub fn with_command_bus(mut self, bus: Arc<dyn CommandBus>) -> Self {
        self.command_bus = bus;
        self
    }

    /// Replace the active audit sink. The default
    /// [`TracingAuditSink`] forwards to `tracing::warn!` under
    /// target `narwhal::plugin::audit`; embedders that want a
    /// programmatic feed pass [`crate::sandbox::RecordingAuditSink`]
    /// or a custom impl.
    #[must_use]
    pub fn with_audit_sink(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = sink;
        self
    }

    /// Replace the active grants. Returns the runtime so calls can
    /// chain off [`Runtime::with_config`].
    #[must_use]
    pub fn with_grants(mut self, grants: Grants) -> Self {
        self.config.grants = grants;
        self
    }

    fn build(
        config: RuntimeConfig,
        log_sink: Arc<dyn LogSink>,
        command_bus: Arc<dyn CommandBus>,
        audit_sink: Arc<dyn AuditSink>,
    ) -> WasmResult<Self> {
        let mut wt_config = Config::new();
        wt_config
            .consume_fuel(true)
            // Wasmtime epoch interruption is the cheaper alternative
            // to fuel checks for long-running plugins. Not used in
            // v0.2 — fuel suffices for our event loop — but turning
            // it on now avoids a Config breaking change later.
            .epoch_interruption(false)
            .wasm_component_model(true);
        let engine = Engine::new(&wt_config).map_err(|e| WasmError::Wasmtime(e.to_string()))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        bindings::narwhal::plugin::host::add_to_linker::<HostState, HostMarker>(
            &mut linker,
            |state: &mut HostState| state,
        )
        .map_err(|e| WasmError::Wasmtime(e.to_string()))?;

        Ok(Self {
            engine,
            linker: Arc::new(linker),
            config,
            log_sink,
            command_bus,
            audit_sink,
        })
    }

    /// Borrow the shared engine. Useful for tests that need to
    /// pre-compile a component without going through
    /// [`Runtime::load`].
    #[must_use]
    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Borrow the active runtime config.
    #[must_use]
    pub const fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Borrow the configured command bus.
    #[must_use]
    pub fn command_bus(&self) -> Arc<dyn CommandBus> {
        Arc::clone(&self.command_bus)
    }

    /// Borrow the active log sink.
    #[must_use]
    pub fn log_sink(&self) -> Arc<dyn LogSink> {
        Arc::clone(&self.log_sink)
    }

    /// Borrow the active audit sink.
    #[must_use]
    pub fn audit_sink(&self) -> Arc<dyn AuditSink> {
        Arc::clone(&self.audit_sink)
    }

    /// Load a plugin given the path to its `plugin.toml`. Wraps the
    /// three discrete steps — manifest parse, policy check, component
    /// instantiate — so callers don't need to thread the
    /// intermediate values themselves.
    pub async fn load(&self, manifest_path: &Path) -> WasmResult<WasmPlugin> {
        let manifest = Manifest::load(manifest_path)?;
        self.load_with_manifest(manifest).await
    }

    /// Load a plugin from a pre-parsed manifest. Lets tests substitute
    /// a synthetic manifest without writing a TOML file to disk.
    pub async fn load_with_manifest(&self, manifest: Manifest) -> WasmResult<WasmPlugin> {
        // 1. API + capability gates fire *before* we read the binary.
        manifest.check_api_version()?;
        // Coarse settings gate: refuse any
        // manifest whose declared capability *kinds* the settings
        // section disallows. We map the manifest set to a Grants
        // shape so the comparison reuses one code path.
        let settings_grants = RuntimeConfig::grants_from_settings(&self.config.settings_policy);
        if let Err(cap) = settings_grants.intersect(&manifest.capabilities) {
            return Err(WasmError::CapabilityDenied {
                capability: cap.to_string(),
            });
        }
        // Fine grants gate: refuse manifests whose declared
        // path/host/var arguments aren't covered by an explicit
        // grant.
        let effective = self
            .config
            .grants
            .intersect(&manifest.capabilities)
            .map_err(|cap| WasmError::CapabilityDenied {
                capability: cap.to_string(),
            })?;

        // 2. Compile the component. `from_file` mmaps the binary so
        // we don't double-buffer the `.wasm` bytes.
        //
        // A pre-flight `exists()` check converts the file-not-found
        // error into a clean [`WasmError::Io`] instead of the raw
        // wasmtime anyhow message, so the user sees the missing path.
        if !manifest.component_path.exists() {
            return Err(WasmError::Io {
                path: manifest.component_path.clone(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }
        let component = Component::from_file(&self.engine, &manifest.component_path)
            .map_err(|e| WasmError::Wasmtime(format!("compile {}: {e}", manifest.name)))?;

        // 3. Build the store, instantiate, run init.
        let limits = build_store_limits(self.config.memory_limit);
        let enforcer = standard_enforcer(
            effective.clone(),
            Arc::clone(&self.audit_sink),
            self.config.grants.broad_cmd,
        );
        let host_state = HostState::new(
            manifest.name.clone(),
            enforcer,
            self.config.kv_budget,
            Arc::clone(&self.log_sink),
            Arc::clone(&self.command_bus),
            limits,
        );

        let mut store = Store::new(&self.engine, host_state);
        store.limiter(|s| s.limiter());
        // Top the store up before the very first export call so init
        // has fuel even if the consumer forgets to refuel.
        store
            .set_fuel(self.config.fuel_per_call)
            .map_err(|e| WasmError::Wasmtime(format!("set_fuel: {e}")))?;

        let bindings =
            bindings::NarwhalPlugin::instantiate_async(&mut store, &component, &self.linker)
                .await
                .map_err(|e| WasmError::Wasmtime(format!("instantiate: {e}")))?;

        // Run init — this is the only place a plugin can refuse to
        // load itself ergonomically (vs trapping during on-event).
        let info = bindings
            .narwhal_plugin_plugin()
            .call_init(&mut store)
            .await
            .map_err(|e| WasmError::Trap {
                name: manifest.name.clone(),
                message: format!("init: {e}"),
            })?
            .map_err(|message| WasmError::Init {
                name: manifest.name.clone(),
                message,
            })?;

        // Plugin-side api-version must match the manifest. Mismatches
        // here are upgrades the author forgot to roll through the
        // manifest — a clear, recoverable error.
        if info.api_version != manifest.api_version {
            return Err(WasmError::Init {
                name: manifest.name.clone(),
                message: format!(
                    "component init declared api-version {} but plugin.toml says {}",
                    info.api_version, manifest.api_version
                ),
            });
        }

        Ok(WasmPlugin::new(manifest, store, bindings, self.clone()))
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Helper used by [`WasmPlugin`] before every export call: refuel the
/// store so each event invocation gets a fresh budget. Surfaces
/// wasmtime errors as [`WasmError::Wasmtime`].
pub(crate) fn refuel(store: &mut Store<HostState>, fuel: u64) -> WasmResult<()> {
    FuelMeter::default().refuel(store, fuel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_defaults() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.memory_limit, DEFAULT_MEMORY_LIMIT);
        assert_eq!(cfg.fuel_per_call, DEFAULT_FUEL_BUDGET);
        assert_eq!(cfg.kv_budget, DEFAULT_KV_BUDGET);
        // Default-deny except state.
        assert!(cfg.grants.allow_state_default);
        assert!(cfg.grants.inner.is_empty());
    }

    #[test]
    fn runtime_builds_with_default_config() {
        let rt = Runtime::new().expect("default runtime should build");
        assert_eq!(rt.config().memory_limit, DEFAULT_MEMORY_LIMIT);
    }

    #[test]
    fn grants_from_settings_maps_flags_to_widest_scope() {
        let mut s = WasmPluginSettings::default();
        s.allow_fs_read = true;
        s.allow_net = true;
        let g = RuntimeConfig::grants_from_settings(&s);
        assert!(g.covers(&Capability::FsRead(PathScope::parse("/etc").unwrap())));
        assert!(g.covers(&Capability::NetConnect(
            HostPort::parse("example.com:443").unwrap()
        )));
        // env/fs-write still denied.
        assert!(!g.covers(&Capability::FsWrite(PathScope::parse("/tmp").unwrap())));
        assert!(!g.covers(&Capability::EnvRead(EnvVar::parse("HOME").unwrap())));
    }
}
