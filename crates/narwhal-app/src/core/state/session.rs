//! Connection-and-data-side state.
//!
//! This sub-state owns the durable, user-facing data the app
//! manipulates: the active session, the saved connection catalogue,
//! the recency cache that drives sidebar ordering, the history
//! journal, the snippet store, and the audit gate.
//!
//! It deliberately excludes:
//! - UI-facing state (tabs, focus, theme — see `UiState`)
//! - Modal overlays (wizard, history modal — see `ModalState`)
//! - Lifecycle / channel plumbing (see `ProcessState`)
//! - Immutable services (registry, plugins, credentials — see `AppDeps`)
//!
//! The `pending_session_opens` set lives here because it tracks
//! which session-id values an `OpenSession` reply is allowed to
//! match against; that is fundamentally session-routing state, not
//! lifecycle state.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use narwhal_config::{ConnectionsFile, LastUsedStore};
use narwhal_history::Journal;
use uuid::Uuid;

use crate::session::Session;
use crate::snippets::SnippetStore;

/// Connection catalogue, active session, and adjacent data
/// (history journal, snippet store, recency cache, audit gate).
pub struct SessionState {
    /// Currently open session, or `None` when no `:open` has run.
    /// The plugin SQL bridge, the sidebar, and the dispatcher all
    /// gate on this being `Some` before doing engine work. Named
    /// `active` to read as `core.session.active` rather than the
    /// stuttering `core.session.session`.
    pub active: Option<Session>,
    /// In-memory copy of `connections.toml`. The wizard mutates
    /// this on commit; `connections_path.is_some()` enables disk
    /// writes (set by `App::with_connections_path`).
    pub connections: ConnectionsFile,
    /// Disk location for `connections.toml`. `None` in test
    /// fixtures and in the headless MCP server.
    pub connections_path: Option<PathBuf>,
    /// Recency cache feeding the sidebar ordering. Populated from
    /// `paths.last_used_file()` on start-up and bumped on every
    /// successful `:open`.
    pub last_used: LastUsedStore,
    /// Disk location for `last_used.toml`. `None` in headless modes.
    pub last_used_path: Option<PathBuf>,
    /// Append-only journal for query history. `None` when the
    /// caller did not pass one in (MCP without `--history`,
    /// tests). All writes are best-effort \u2014 a broken journal does
    /// not block query execution.
    pub history_journal: Option<Arc<Journal>>,
    /// On-disk snippet catalogue. The modal that picks from this
    /// lives in `ModalState::snippets`; this is the data source.
    pub snippet_store: SnippetStore,
    /// `ConnectionConfig.id`s for which an `OpenSession` meta
    /// request is currently in flight. Used to drop stale
    /// `MetaUpdate::SessionOpened` replies (user opened another
    /// connection, closed the active one) before they clobber the
    /// current state. (Bug H7 fix.)
    pub pending_session_opens: HashSet<Uuid>,
    /// Global read-only switch. When `true`, every row-CRUD entry
    /// point refuses to stage mutations regardless of the driver's
    /// `row_level_dml` capability. Driven by the `--read-only`
    /// CLI flag (and by the MCP server's audit gate).
    pub read_only: bool,
    /// v1.3 #11: filter string captured by `:history <pattern>` that
    /// the meta-channel completion handler applies once the journal
    /// entries arrive. Cleared on apply.
    pub pending_history_filter: Option<String>,
}

impl SessionState {
    /// Construct an empty `SessionState`. The connection catalogue
    /// is supplied here because the caller usually has it (either
    /// freshly loaded from disk or built in-memory by a test);
    /// every other field starts in its \"nothing yet\" form.
    pub fn new(connections: ConnectionsFile, history_journal: Option<Arc<Journal>>) -> Self {
        Self {
            active: None,
            connections,
            connections_path: None,
            last_used: LastUsedStore::default(),
            last_used_path: None,
            history_journal,
            snippet_store: SnippetStore::new(SnippetStore::default_root()),
            pending_session_opens: HashSet::new(),
            read_only: false,
            pending_history_filter: None,
        }
    }
}
