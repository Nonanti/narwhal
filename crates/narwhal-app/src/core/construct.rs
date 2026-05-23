//! AppCore constructors and settings application.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_history::Journal;
use narwhal_plugin::PluginRegistry;
use narwhal_tui::{LayoutRegions, Pane, Theme};
use narwhal_vim::Vim;
use tokio::sync::{mpsc, Mutex};

use super::plugin_executor::PluginConnectionState;
use super::{AppCore, SidebarItem, StatusBar, Tab};
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
        credentials: Arc<dyn CredentialStore>,
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
        credentials: Arc<dyn CredentialStore>,
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
    pub fn clipboard(&self) -> Arc<dyn Clipboard> {
        Arc::clone(&self.clipboard)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        registry: DriverRegistry,
        connections: ConnectionsFile,
        history: Option<Arc<Journal>>,
        credentials: Arc<dyn CredentialStore>,
        clipboard: Arc<dyn Clipboard>,
        run_tx: mpsc::Sender<RunUpdate>,
        run_rx: mpsc::Receiver<RunUpdate>,
        meta_tx: mpsc::Sender<MetaUpdate>,
        meta_rx: mpsc::Receiver<MetaUpdate>,
    ) -> Self {
        Self {
            registry,
            connections,
            connections_path: None,
            last_used: narwhal_config::LastUsedStore::new(),
            last_used_path: None,
            credentials,
            clipboard,
            plugins: {
                let mut reg = PluginRegistry::new();
                reg.reserve_builtins(crate::commands::BUILTIN_COMMAND_NAMES.iter().copied());
                Arc::new(reg)
            },
            plugin_state: Arc::new(std::sync::Mutex::new(PluginConnectionState::default())),
            history_journal: history,
            history_state: None,
            snippet_store: SnippetStore::new(SnippetStore::default_root()),
            snippets_modal: None,
            session: None,
            tabs: vec![Tab::new("untitled-1")],
            active_tab: 0,
            next_tab_id: 2,
            vim: Vim::new(),
            theme: Theme::default(),
            focus: Pane::Editor,
            sidebar_items: Vec::new(),
            sidebar_index: 0,
            sidebar_scroll: 0,
            status: StatusBar {
                message: "ready".into(),
                ..Default::default()
            },
            plugin_warning: None,
            running: false,
            run_tab: None,
            cancel_slot: Arc::new(Mutex::new(None)),
            should_quit: false,
            wizard: None,
            wizard_error: None,
            help_open: false,
            pending_result_leader: None,
            pending_result_entries_states: Vec::new(),
            pending_result_entries_views: Vec::new(),
            last_layout: LayoutRegions::default(),
            run_tx,
            run_rx,
            meta_tx,
            meta_rx,
            refresh_task: None,
            refresh_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Inform the core where to persist new connections produced by the
    /// `:add` wizard. Called by [`crate::app::App::new`].
    pub fn set_connections_path(&mut self, path: std::path::PathBuf) {
        self.connections_path = Some(path);
    }

    /// Wire the recency cache into the on-disk file produced by
    /// `ConfigPaths::last_used_file()`. Existing entries are loaded
    /// immediately so the very first sidebar render reflects the
    /// previous session's ordering.
    pub fn set_last_used_path(&mut self, path: std::path::PathBuf) {
        if let Ok(loaded) = narwhal_config::LastUsedStore::load(&path) {
            self.last_used = loaded;
        }
        self.last_used_path = Some(path);
        self.rebuild_sidebar();
    }

    /// Record that `id` was just opened: bumps the in-memory cache and
    /// best-effort writes it to disk. Failures are logged at debug and
    /// not surfaced — ordering is a UX nicety, not load-bearing.
    pub(super) fn touch_last_used(&mut self, id: uuid::Uuid) {
        self.last_used.touch(id);
        if let Some(path) = self.last_used_path.as_ref() {
            if let Err(error) = self.last_used.save(path) {
                tracing::debug!(target: "narwhal::app", error = %error, "last-used save failed");
            }
        }
    }

    /// Override the snippet store root directory. Used by tests to
    /// avoid polluting the user's real config.
    #[doc(hidden)]
    pub fn set_snippet_store_root(&mut self, root: std::path::PathBuf) {
        self.snippet_store = SnippetStore::new(root);
    }

    /// Apply a user-supplied [`narwhal_config::Settings`] payload.
    ///
    /// Currently honoured: [`narwhal_config::Theme`] is mapped onto the
    /// renderer's [`Theme`] palette. The `editor` / `keybindings`
    /// sections are accepted for forward compatibility but do not yet
    /// influence runtime behaviour — the load-then-warn surface alone
    /// catches malformed `config.toml` files at start-up so we never
    /// fall back to defaults blindly.
    pub fn apply_settings(&mut self, settings: narwhal_config::Settings) {
        self.theme = match settings.theme {
            narwhal_config::Theme::Dark => Theme::DARK,
            narwhal_config::Theme::Light => Theme::LIGHT,
            narwhal_config::Theme::HighContrast => Theme::HIGH_CONTRAST,
            // `narwhal_config::Theme` is `#[non_exhaustive]`; future
            // variants fall back to DARK rather than refusing to start.
            _ => Theme::DARK,
        };
    }

    pub(super) fn rebuild_sidebar(&mut self) {
        let mut items = Vec::new();
        let active_id = self.session.as_ref().map(|s| s.config.id);
        // Show most-recently-opened connections first; ties (or
        // never-opened entries) fall back to alphabetical order so the
        // list is stable across reboots.
        let mut ordered: Vec<&narwhal_core::ConnectionConfig> =
            self.connections.connections.iter().collect();
        ordered.sort_by(|a, b| {
            let ta = self.last_used.get(a.id).unwrap_or(0);
            let tb = self.last_used.get(b.id).unwrap_or(0);
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
                if let Some(session) = self.session.as_ref() {
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
        self.sidebar_items = items;
        if self.sidebar_index >= self.sidebar_items.len() {
            self.sidebar_index = self.sidebar_items.len().saturating_sub(1);
        }
    }
}
