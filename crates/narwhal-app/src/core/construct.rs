//! `AppCore` constructors and settings application.

use std::sync::Arc;

use narwhal_config::{ConnectionsFile, DynCredentialStore, InMemoryStore, VaultRegistry};
use narwhal_history::Journal;
use narwhal_plugin::PluginRegistry;
use narwhal_tui::Theme;
use tokio::sync::{Mutex, mpsc};

use super::plugin_executor::PluginConnectionState;
use super::{AppCore, SidebarItem};
use crate::clipboard::{Clipboard, InMemoryClipboard};
use crate::meta::MetaUpdate;
use crate::registry::DriverRegistry;
use crate::run::RunUpdate;
use crate::snippets::SnippetStore;

const RUN_CHANNEL_CAPACITY: usize = 128;

impl AppCore {
    pub fn new(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
    ) -> Self {
        Self::with_credentials(
            registry,
            connections,
            history,
            Arc::new(InMemoryStore::new()),
        )
    }

    /// Same as [`Self::new`] but lets the caller inject a credential store.
    /// Production builds pass a [`narwhal_config::KeyringStore`]; tests use
    /// [`InMemoryStore`].
    pub fn with_credentials(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn DynCredentialStore>,
    ) -> Self {
        Self::with_services(
            registry,
            connections,
            history,
            credentials,
            Arc::new(InMemoryClipboard::new()),
        )
    }

    /// Inject every replaceable runtime service in one call. The binary
    /// passes [`narwhal_config::KeyringStore`] and
    /// [`crate::clipboard::ArboardClipboard`]; tests pass the in-memory
    /// variants.
    pub fn with_services(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn DynCredentialStore>,
        clipboard: Arc<dyn Clipboard>,
    ) -> Self {
        let (run_tx, run_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        let (meta_tx, meta_rx) = mpsc::channel(RUN_CHANNEL_CAPACITY);
        let mut this = Self::new_inner(
            registry,
            connections,
            history,
            credentials,
            clipboard,
            run_tx,
            run_rx,
            meta_tx,
            meta_rx,
        );
        this.rebuild_sidebar();
        this
    }

    /// Read-only accessor for the active clipboard. Mostly useful for
    /// tests that want to assert what was just yanked.
    pub async fn clipboard(&self) -> Arc<dyn Clipboard> {
        Arc::clone(&self.deps.clipboard)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn DynCredentialStore>,
        clipboard: Arc<dyn Clipboard>,
        run_tx: mpsc::Sender<RunUpdate>,
        run_rx: mpsc::Receiver<RunUpdate>,
        meta_tx: mpsc::Sender<MetaUpdate>,
        meta_rx: mpsc::Receiver<MetaUpdate>,
    ) -> Self {
        Self {
            // AppDeps bundles every immutable service: driver
            // registry, credential store, clipboard, plugin
            // registry + state, keymap. Tests build one of these
            // to mock the entire I/O boundary.
            deps: super::AppDeps {
                registry,
                credentials,
                vault: Arc::new(VaultRegistry::empty()),
                clipboard,
                plugins: {
                    let mut reg = PluginRegistry::new();
                    reg.reserve_builtins(crate::commands::BUILTIN_COMMAND_NAMES.iter().copied());
                    Arc::new(reg)
                },
                plugin_state: Arc::new(std::sync::Mutex::new(PluginConnectionState::default())),
                keymap: crate::keymap::Keymap::builtin(),
            },
            // ModalState::default() = every modal closed, every
            // option None, help_open=false. Bundled so callers
            // don't have to know which fields exist.
            modals: super::ModalState::default(),
            // SessionState bundles the connection catalogue, active
            // session, recency cache, history journal, snippet
            // store, audit gate, and pending-open ledger.
            session: super::SessionState::new(connections, history),
            // UiState::new() = one untitled tab, editor focused,
            // empty sidebar, default theme, status "ready".
            ui: super::UiState::new(),
            // ProcessState bundles every lifecycle / async-bridge
            // field. Receivers stay outside it (see mod.rs comment).
            process: super::ProcessState::new(run_tx, meta_tx, Arc::new(Mutex::new(None))),
            run_rx,
            meta_rx,
            keymap_warnings: Vec::new(),
            last_self_settings_write: None,
        }
    }

    /// Inform the core where to persist new connections produced by the
    /// `:add` wizard. Called by [`crate::app::App::new`].
    pub fn set_connections_path(&mut self, path: std::path::PathBuf) {
        self.session.connections_path = Some(path);
    }

    /// Wire the recency cache into the on-disk file produced by
    /// `ConfigPaths::last_used_file()`. Existing entries are loaded
    /// immediately so the very first sidebar render reflects the
    /// previous session's ordering.
    pub fn set_last_used_path(&mut self, path: std::path::PathBuf) {
        if let Ok(loaded) = narwhal_config::LastUsedStore::load(&path) {
            self.session.last_used = loaded;
        }
        self.session.last_used_path = Some(path);
        self.rebuild_sidebar();
    }

    /// Record that `id` was just opened: bumps the in-memory cache and
    /// best-effort writes it to disk. Failures are logged at debug and
    /// not surfaced — ordering is a UX nicety, not load-bearing.
    pub(super) fn touch_last_used(&mut self, id: uuid::Uuid) {
        self.session.last_used.touch(id);
        if let Some(path) = self.session.last_used_path.as_ref() {
            if let Err(error) = self.session.last_used.save(path) {
                tracing::debug!(target: "narwhal::app", error = %error, "last-used save failed");
            }
        }
    }

    /// Override the snippet store root directory. Used by tests to
    /// avoid polluting the user's real config.
    #[doc(hidden)]
    pub fn set_snippet_store_root(&mut self, root: std::path::PathBuf) {
        self.session.snippet_store = SnippetStore::new(root);
    }

    /// L36 #11: enter / leave read-only mode. When `on` is true every
    /// row-CRUD entry point bails with an explanatory status message
    /// before staging any mutation.
    pub const fn set_read_only(&mut self, on: bool) {
        self.session.read_only = on;
    }

    /// Install the audit service. Default is `None` — emit
    /// sites become no-ops. The binary constructs the service from
    /// `settings.audit` and calls this method during startup; tests
    /// usually leave it `None`.
    pub fn set_audit_service(&mut self, svc: Arc<narwhal_audit::AuditService>) {
        self.session.audit_service = Some(svc);
    }

    /// Swap in a configured [`VaultRegistry`].
    ///
    /// Default state after [`Self::with_services`] is an empty
    /// registry, which means `password = "vault:…"` references in
    /// `connections.toml` will fail with
    /// [`narwhal_config::VaultError::NotConfigured`]. The binary
    /// constructs the populated registry from
    /// `settings.vault.providers` and calls this method before the
    /// first `:open`.
    pub fn set_vault(&mut self, vault: Arc<VaultRegistry>) {
        self.deps.vault = vault;
    }

    /// Apply a user-supplied [`narwhal_config::Settings`] payload.
    ///
    /// Wires the live host state to every section the user can edit
    /// from `settings.toml` or the in-app `:settings` modal:
    /// theme palette, diagram glyphs, streaming tuning, editor
    /// input model, mouse behaviour, mode indicator and the
    /// `[keymap.*]` overrides.
    ///
    /// The deprecated `keybindings.vim_mode = false` knob is
    /// translated into `editor.mode = Basic` here so v1 configs
    /// keep working without forcing a `migrate-config` pass.
    pub fn apply_settings(&mut self, settings: narwhal_config::Settings) {
        self.ui.diagram_icons = settings.diagram.icons;
        // streaming-result tuning. Defended in StreamTuning::new
        // so a misconfigured `batch_size = 0` cannot livelock the worker.
        self.session.stream_tuning =
            crate::run::StreamTuning::new(settings.run.batch_size, settings.run.stream_flush_ms);
        self.ui.theme = match settings.theme {
            narwhal_config::Theme::Dark => Theme::DARK,
            narwhal_config::Theme::Light => Theme::LIGHT,
            narwhal_config::Theme::HighContrast => Theme::HIGH_CONTRAST,
            // `narwhal_config::Theme` is `#[non_exhaustive]`; future
            // variants fall back to DARK rather than refusing to start.
            _ => Theme::DARK,
        };

        // Editor input model. The deprecated `vim_mode = false` bit
        // wins only when the user did not also set `editor.mode`
        // explicitly — a v1.x config flips to Basic without a
        // migration pass, while a v2.x config that sets both keeps
        // its explicit `editor.mode` choice.
        let prev_mode = self.ui.editor_mode;
        self.ui.editor_mode = settings.editor.mode;
        #[allow(deprecated)]
        if !settings.keybindings.vim_mode && settings.editor.mode == narwhal_config::EditorMode::Vim
        {
            self.ui.editor_mode = narwhal_config::EditorMode::Basic;
        }
        // FIX V2: Reset the vim state machine when the editor mode
        // changes at runtime. Without this, switching from Vim to Basic
        // while in Command mode leaves keystrokes routing through the
        // dead vim handler.
        if prev_mode != self.ui.editor_mode {
            self.ui.vim = narwhal_vim::Vim::new();
        }
        self.ui.mouse_mode = settings.editor.mouse;
        self.ui.show_mode_indicator = settings.editor.show_mode_indicator;
        self.ui.key_preset = settings.keybindings.preset;

        // turn the `[keymap.<group>]` TOML sections into a typed
        // override table, apply on top of the built-in defaults, and
        // stash diagnostics so the first render surfaces them.
        //
        // FIX B1+B8: Rebuild from builtin defaults on every reload so
        // that removed user overrides no longer persist, and clear the
        // warnings vec so diagnostics don't accumulate across reloads.
        self.deps.keymap = crate::keymap::Keymap::builtin();
        self.keymap_warnings.clear();
        if !settings.keymap.is_empty() {
            let mut typed: std::collections::HashMap<
                crate::action::KeyGroup,
                std::collections::HashMap<String, String>,
            > = std::collections::HashMap::new();
            for (raw_group, table) in &settings.keymap {
                if let Some(group) = crate::action::KeyGroup::from_str_opt(raw_group) {
                    typed.insert(group, table.clone());
                } else {
                    self.keymap_warnings
                        .push(format!("unknown keymap group: '{raw_group}'"));
                }
            }
            let diags = self.deps.keymap.apply_overrides(&typed);
            for d in diags {
                self.keymap_warnings.push(d.to_string());
            }
        }
    }

    pub(super) fn rebuild_sidebar(&mut self) {
        let mut items = Vec::new();
        let active_id = self.session.active.as_ref().map(|s| s.config.id);
        // Show most-recently-opened connections first; ties (or
        // never-opened entries) fall back to alphabetical order so the
        // list is stable across reboots.
        let mut ordered: Vec<&narwhal_core::ConnectionConfig> =
            self.session.connections.connections.iter().collect();
        ordered.sort_by(|a, b| {
            let ta = self.session.last_used.get(a.id).unwrap_or(0);
            let tb = self.session.last_used.get(b.id).unwrap_or(0);
            tb.cmp(&ta).then_with(|| a.name.cmp(&b.name))
        });
        for conn in ordered {
            let active = Some(conn.id) == active_id;
            items.push(SidebarItem::Connection {
                id: conn.id,
                name: conn.name.clone(),
                driver: conn.driver.clone(),
                active,
            });
            if active {
                if let Some(session) = self.session.active.as_ref() {
                    for (schema, tables) in &session.schemas {
                        if !schema.name.is_empty() {
                            items.push(SidebarItem::Schema {
                                name: schema.name.clone(),
                            });
                        }
                        for table in tables {
                            items.push(SidebarItem::Table {
                                schema: table.schema.clone(),
                                name: table.name.clone(),
                                kind: table.kind,
                            });
                        }
                    }
                }
            }
        }
        self.ui.sidebar_items = items;
        if self.ui.sidebar_index >= self.ui.sidebar_items.len() {
            self.ui.sidebar_index = self.ui.sidebar_items.len().saturating_sub(1);
        }
    }
}
