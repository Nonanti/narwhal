//! Wire-format types for the workspace-state snapshot.
//!
//! The split between these types and the live [`crate::core::state`]
//! structs is intentional: the runtime types hold `Arc`s, channels and
//! parser handles that don't serialise; the persisted projection
//! captures only the user-visible "where was I?" surface.
//!
//! All fields are tagged `#[serde(default, skip_serializing_if = ...)]`
//! where appropriate so a TOML round-trip stays terse for the common
//! "empty session" case. Top-level structs are `#[non_exhaustive]`
//! so future fields land non-breakingly.
//!
//! Schema version is `1` for v2.0. A bump (any structural change to a
//! field name or type) goes through the `migrate` helper in
//! the load/apply path in [`super`] rather than relying on TOML's
//! forgiving unknown-field handling.

use serde::{Deserialize, Serialize};

/// Schema version produced and accepted by the current binary.
///
/// Bumped on any *breaking* structural change to the persisted
/// snapshot. Adding optional fields (default-able, `#[serde(default)]`)
/// does not require a bump — TOML drops unknown keys cleanly on the
/// reader side and writers always emit the current shape.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Top-level workspace snapshot persisted at clean exit.
///
/// The on-disk file (`~/.config/narwhal/workspace-state.toml`) is a
/// flat TOML document with `schema_version = N` as the first key
/// followed by the fields below. Empty optionals collapse on
/// serialisation to keep the file readable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct PersistedWorkspace {
    /// Snapshot wire-format version. Always
    /// [`CURRENT_SCHEMA_VERSION`] on writes. A future binary that
    /// reads an older value runs the migration ladder in
    /// [`super::load_at_start`]; a newer value than the
    /// binary supports is logged and skipped (forward-compat is
    /// best-effort, not guaranteed).
    pub schema_version: u32,
    /// Narwhal version string captured at write time. Surfaced in
    /// debug logs when a forward-skew warning fires; never gates
    /// behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narwhal_version: Option<String>,
    /// ISO-8601 timestamp of the snapshot write. Persisted for human
    /// debugging only — restore doesn't gate on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
    /// Name of the connection that was active when the snapshot was
    /// taken. Resolved at load time against `connections.toml`; a
    /// rename or removal silently degrades to "no active connection".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_connection: Option<String>,
    /// Editor tabs, source-ordered. Empty when the user had only
    /// the implicit untitled tab open with no edits — restore in
    /// that case leaves the default tab list alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PersistedTab>,
    /// Index of the focused tab inside [`Self::tabs`]. Clamped at
    /// restore-time so a malformed file can't drive an out-of-bounds
    /// index into runtime state.
    pub active_tab: usize,
    /// Sidebar viewport state. All fields default to zero / empty,
    /// matching the "first run" state the UI ships with.
    pub sidebar: PersistedSidebar,
}

/// One editor tab as restored on next launch.
///
/// Tracks the buffer text, the caret position, the vertical scroll
/// offset, and a tag describing what the tab represents. Result
/// tabs (snippets browser, history modal results) are not stored
/// here — only persistent SQL editor tabs round-trip; transient
/// modals re-open on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct PersistedTab {
    /// Display name shown in the tab bar.
    pub name: String,
    /// Verbatim editor buffer contents. May contain secrets the
    /// user pasted in; documented as plaintext in the persist
    /// module-level docs.
    pub buffer: String,
    /// Caret row (0-indexed). Restored via
    /// [`narwhal_domain::editor::EditorBuffer::set_cursor`] so an
    /// out-of-bounds value clamps to the last line rather than
    /// panicking.
    pub cursor_row: usize,
    /// Caret column as a byte offset into the cursor row. Snapped
    /// to the nearest char boundary at restore time so multibyte
    /// content stays safe.
    pub cursor_col: usize,
    /// Vertical scroll offset (first visible row). Independent of
    /// the cursor row — preserves the user's "I was reading line
    /// 200 with cursor on 240" viewport on restart.
    pub scroll: usize,
    /// Tab kind discriminant. v2.0 only ships
    /// [`PersistedTabKind::SqlEditor`]; the enum is
    /// `#[non_exhaustive]` so the snippets-browser or history-tab
    /// pinning follow-ups can extend it without a schema bump.
    pub kind: PersistedTabKind,
}

/// Tag describing what the persisted tab represents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PersistedTabKind {
    /// SQL editor buffer with a single contiguous text body. The
    /// only variant v2.0 actually round-trips.
    #[default]
    SqlEditor,
}

/// Sidebar viewport state captured for restore.
///
/// `expanded_schemas` is a forward-compatibility hook: v2.0 sidebar
/// items always render flat (every schema's tables visible), so the
/// list is empty in practice and the field is reserved for the
/// collapsible-schemas follow-up tracked under T2-T3 backlog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct PersistedSidebar {
    /// Highlighted-row index in the flattened sidebar listing.
    pub selected_index: usize,
    /// First visible row in the sidebar viewport.
    pub scroll: usize,
    /// Schema names the user has explicitly expanded. Empty in
    /// v2.0 (collapsible schemas not yet implemented).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expanded_schemas: Vec<String>,
}

impl PersistedWorkspace {
    /// Construct an empty snapshot tagged with the current schema
    /// version. The shutdown projection in [`super::snapshot`]
    /// fills in every other field.
    pub fn empty() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            narwhal_version: None,
            saved_at: None,
            active_connection: None,
            tabs: Vec::new(),
            active_tab: 0,
            sidebar: PersistedSidebar::default(),
        }
    }

    /// Every public `#[non_exhaustive]` struct ships a
    /// `with(|p| …)` builder so downstream code (tests, MCP tooling)
    /// can construct an instance without depending on the field set.
    /// Mirrors `ConnectionParams::with` and friends.
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut this = Self::empty();
        f(&mut this);
        this
    }
}

impl PersistedTab {
    /// Builder constructor; see [`PersistedWorkspace::with`].
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut this = Self::default();
        f(&mut this);
        this
    }
}

impl PersistedSidebar {
    /// Builder constructor; see [`PersistedWorkspace::with`].
    pub fn with(f: impl FnOnce(&mut Self)) -> Self {
        let mut this = Self::default();
        f(&mut this);
        this
    }
}
