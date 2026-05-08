//! T1-T3-B — Bridge between [`AppCore`]'s private state and the
//! `persist` module's wire-format types.
//!
//! Lives in `core/` because the projection and restore both need
//! direct read/write access to `AppCore`'s internal sub-states
//! (`UiState`, `SessionState`), which `pub(super)` keeps inside the
//! `core` module tree. The `persist` module re-exports the two
//! free functions defined here as `persist::snapshot` and
//! `persist::apply`.
//!
//! ## Treesitter cache interaction
//!
//! `Tab::ts_parser` and `Tab::sql_highlights` are *not* persisted —
//! they're per-process caches owned by the live parser and would
//! never round-trip safely (raw C pointers inside `tree_sitter::Parser`).
//! Restore rebuilds tabs from buffer text only, leaving the cache
//! fields at their `None` defaults. The first render after restore
//! triggers a fresh `Tab::sql_highlights` call which re-populates
//! the cache — exactly the same path a freshly-typed buffer takes.
//! See `docs/dev/t1-t3-a-treesitter.md`, "Cache policy" — the
//! length-keyed invalidation contract is preserved: spans are
//! recomputed whenever the buffer length differs from the cached
//! value, which is trivially true for a freshly-restored tab
//! (cached length starts at zero).
//!
//! ## What we deliberately don't restore
//!
//! - **Result bundles**: per the brief, re-running on demand is
//!   cheap; persisting 100k cached rows is not.
//! - **Modal overlays** (wizard, history, snippets picker, json
//!   viewer, diagram modal, pending-preview): all transient by
//!   construction — the user invoked them, the user can re-invoke
//!   them.
//! - **Vim transient state** (pending leader keys, command-line
//!   buffer): mid-input snapshots are user-hostile.
//! - **Plugin pool / pending session opens**: lifecycle state owned
//!   by `ProcessState`; restoring it would race the fresh
//!   `AppCore::new` wiring.

use narwhal_config::WorkspacePersistSettings;
use narwhal_domain::editor::EditorBuffer;

use super::AppCore;
use super::state::Tab;
use crate::persist::schema::{
    CURRENT_SCHEMA_VERSION, PersistedSidebar, PersistedTab, PersistedTabKind, PersistedWorkspace,
};

/// Read-side projection: borrow the live [`AppCore`] state and
/// build a wire-format snapshot. Pure function — never mutates.
pub(crate) fn project_workspace(core: &AppCore) -> PersistedWorkspace {
    let tabs = core
        .ui
        .tabs
        .iter()
        .map(project_tab)
        .collect::<Vec<PersistedTab>>();
    let active_tab = core.ui.active_tab.min(tabs.len().saturating_sub(1));

    PersistedWorkspace {
        schema_version: CURRENT_SCHEMA_VERSION,
        narwhal_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        saved_at: Some(current_timestamp()),
        active_connection: core.session.active.as_ref().map(|s| s.config.name.clone()),
        tabs,
        active_tab,
        sidebar: PersistedSidebar {
            selected_index: core.ui.sidebar_index,
            scroll: core.ui.sidebar_scroll,
            expanded_schemas: Vec::new(),
        },
    }
}

fn project_tab(tab: &Tab) -> PersistedTab {
    let (cursor_row, cursor_col) = tab.editor.cursor();
    PersistedTab {
        name: tab.name.clone(),
        buffer: tab.editor.entire_text(),
        cursor_row,
        cursor_col,
        scroll: tab.editor.scroll(),
        kind: PersistedTabKind::SqlEditor,
    }
}

/// Write-side restore: mutate the [`AppCore`] to reflect `snapshot`.
///
/// Returns the name of the connection to re-open asynchronously, or
/// `None` when no connection was active, restore is disabled, or
/// the snapshot is empty enough that there's nothing to do.
///
/// Honours [`WorkspacePersistSettings`] field-by-field so users
/// can mix-and-match (restore connection only, restore tabs only,
/// etc.).
pub(crate) fn apply_workspace(
    core: &mut AppCore,
    snapshot: PersistedWorkspace,
    settings: &WorkspacePersistSettings,
) -> Option<String> {
    if !settings.enabled {
        return None;
    }

    if settings.restore_tabs && !snapshot.tabs.is_empty() {
        restore_tabs(core, &snapshot, settings.restore_cursor);
    }

    if settings.restore_sidebar {
        restore_sidebar(core, &snapshot.sidebar);
    }

    // The connection re-open is async — return the name so the
    // binary's startup task can fire `:open <name>` once the
    // runtime is alive. We return it unconditionally on
    // `enabled = true` because a saved connection is the single
    // most-valuable thing to restore; there's no separate
    // `restore_connection` toggle in v2.0.
    snapshot.active_connection
}

/// Replace `core.ui.tabs` with the persisted tabs. Cursor and
/// scroll are re-applied through [`EditorBuffer::set_cursor`] /
/// [`EditorBuffer::set_scroll`] so the clamping invariants stay
/// intact (out-of-bounds values from a malformed file or schema
/// drift cap silently rather than panicking).
///
/// `restore_cursor` toggles per-tab cursor placement: when `false`
/// the buffer text still restores but every tab reopens at
/// `(row 0, col 0, scroll 0)`. Users who want "reopen my queries
/// but start me at the top of each" enable `restore_tabs` and leave
/// `restore_cursor = false`.
fn restore_tabs(core: &mut AppCore, snapshot: &PersistedWorkspace, restore_cursor: bool) {
    let mut next_id: u64 = 1;
    let mut tabs = Vec::with_capacity(snapshot.tabs.len());
    for persisted in &snapshot.tabs {
        let mut tab = Tab::new(next_id, persisted.name.clone());
        next_id = next_id.saturating_add(1);
        rebuild_editor(&mut tab.editor, persisted, restore_cursor);
        tabs.push(tab);
    }
    core.ui.tabs = tabs;
    // Clamp the active-tab index into the restored list. An empty
    // snapshot list was rejected at the call-site, so `tabs` is
    // guaranteed non-empty here.
    core.ui.active_tab = snapshot
        .active_tab
        .min(core.ui.tabs.len().saturating_sub(1));
    // Bump `next_tab_id` past the highest restored id so the next
    // `:new-tab` allocates a stable, non-colliding handle.
    core.ui.next_tab_id = next_id as usize;
}

/// Reset `editor` to a clean state, push the persisted buffer back
/// in, then position the cursor and viewport. The two-step
/// `clear` + `insert_str` keeps us on the public `EditorBuffer`
/// API (no direct field access from outside `narwhal-domain`).
fn rebuild_editor(editor: &mut EditorBuffer, persisted: &PersistedTab, restore_cursor: bool) {
    editor.clear();
    if !persisted.buffer.is_empty() {
        editor.insert_str(&persisted.buffer);
    }
    if restore_cursor {
        // Cursor first, then scroll. `set_cursor` clamps row/col
        // and snaps the column to a char boundary; `set_scroll` is
        // a raw setter and trusts the caller — fine here because
        // the cache is value-clamped on read by the renderer
        // anyway.
        editor.set_cursor(persisted.cursor_row, persisted.cursor_col);
        editor.set_scroll(persisted.scroll);
    } else {
        // `insert_str` left the caret at end-of-buffer; rewind to
        // (0, 0) so the user's view matches the "fresh open"
        // expectation when they explicitly opted out of cursor
        // restore.
        editor.set_cursor(0, 0);
        editor.set_scroll(0);
    }
}

/// Reapply the sidebar viewport state. Selection and scroll are
/// clamped against the freshly-rebuilt sidebar listing on the next
/// frame (`AppCore::rebuild_sidebar` does the bound check); we
/// trust the user-visible behaviour to do the right thing here.
const fn restore_sidebar(core: &mut AppCore, persisted: &PersistedSidebar) {
    core.ui.sidebar_index = persisted.selected_index;
    core.ui.sidebar_scroll = persisted.scroll;
    // `expanded_schemas` is reserved for the collapsible-sidebar
    // follow-up; v2.0 keeps the field in the snapshot for forward
    // compat but the runtime sidebar is flat, so nothing to apply.
}

impl AppCore {
    /// T1-T3-B: kick the dispatcher down the same path `:open NAME`
    /// would have taken, on behalf of the workspace-state restore.
    /// Exposed as a `pub(crate)` method so the binary's startup
    /// task (in `App::run`) doesn't have to grow visibility on
    /// the private `open_named` helper.
    ///
    /// The dispatcher's existing "connection not found" / "driver
    /// not registered" handling does the right thing for a
    /// renamed-or-removed connection (status-bar warning, no
    /// session installed) so there's no extra error plumbing here.
    pub(crate) async fn reopen_restored_connection(&mut self, name: &str) {
        self.open_named(name).await;
    }
}

/// Best-effort ISO-8601 timestamp without pulling `chrono` into
/// `narwhal-app`. The seconds-since-epoch fallback keeps the
/// snapshot useful for debugging even when the real wall-clock
/// formatter (which `chrono` would provide) is unavailable.
fn current_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{}s-since-epoch", d.as_secs()),
        Err(_) => "unknown".to_string(),
    }
}
