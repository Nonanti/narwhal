//! One editor tab: name, buffer, run state, results.

use narwhal_tui::EditorBuffer;

use super::result::{
    CellEdit, CompletionState, DiagramModalState, EditorSearchState, JsonViewerState, ResultBundle,
    ResultSearch, RowDetailState, RowSource,
};
use crate::pending::PendingChanges;

pub struct Tab {
    /// Stable, monotonically-assigned identifier. Used by the meta
    /// channel to address replies to the originating tab even after
    /// other tabs are closed (which shifts indices). Initial tab is
    /// `1`; `new_tab` allocates from `AppCore::next_tab_id`. Wraps at
    /// `u64::MAX` — well beyond any plausible session.  (Bug C5 fix.)
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) editor: EditorBuffer,
    pub(crate) results: ResultBundle,
    pub(crate) search: Option<ResultSearch>,
    pub(crate) editing: Option<CellEdit>,
    pub(crate) completion: Option<CompletionState>,
    /// Per-tab editor search state (separate from result pane search).
    pub(crate) editor_search: EditorSearchState,
    /// Page size used by the next sidebar preview. Stored per-tab so a
    /// user paging through one table doesn't disturb another tab.
    pub(crate) page_size: usize,
    /// Pending row source to attach to the next `Rows` result. Populated
    /// by `preview_sidebar_selection` and consumed in `finish_run`.
    pub(crate) pending_source: Option<RowSource>,
    /// When `Some`, the row detail modal is open on the result pane.
    /// Sits at the same layer as the cell popup; only one of them
    /// should be open at a time.
    pub(crate) row_detail: Option<RowDetailState>,
    /// When `Some`, the JSON viewer modal (L36) is open. Stacks above
    /// every other result-pane overlay; receives every key until
    /// dismissed with `q`/`Esc`.
    pub(crate) json_viewer: Option<JsonViewerState>,
    /// When `Some`, the diagram modal (Focused or Impact) is open.
    /// Sits below the JSON viewer in the modal stack but above the
    /// pending preview, so a diagram opened while a cell-popup is up
    /// becomes the active overlay.
    pub(crate) diagram: Option<DiagramModalState>,
    /// L36: staged row-level mutations awaiting commit. Persists for
    /// the lifetime of the tab; the user dismisses it with Ctrl-X or
    /// commits it with Ctrl-S. Cross-table batches are explicitly
    /// allowed — useful for fixing foreign-key chains in one
    /// transaction.
    pub(crate) pending: PendingChanges,
    /// When `Some`, the pending-preview modal is open. The state is
    /// minimal (just a scroll cursor); the body is reconstructed from
    /// `pending` every render.
    pub(crate) pending_preview: Option<PendingPreviewState>,
}

/// Lightweight modal state for the pending-preview overlay. Only
/// carries the scroll cursor; the body comes from the live
/// [`PendingChanges`] queue at render time so commits/discards reflect
/// immediately.
#[derive(Debug, Clone, Default)]
pub struct PendingPreviewState {
    pub scroll: u16,
}

impl Tab {
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            editor: EditorBuffer::new(),
            results: ResultBundle::default(),
            search: None,
            editing: None,
            completion: None,
            editor_search: EditorSearchState::default(),
            page_size: 100,
            pending_source: None,
            row_detail: None,
            json_viewer: None,
            diagram: None,
            pending: PendingChanges::new(),
            pending_preview: None,
        }
    }

    /// Stable identifier (see field doc).
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Tab display name shown in the tab bar.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Editor buffer attached to this tab.
    pub const fn editor(&self) -> &EditorBuffer {
        &self.editor
    }

    /// Mutable editor buffer for tests and host-side compositors.
    pub fn editor_mut(&mut self) -> &mut EditorBuffer {
        &mut self.editor
    }

    /// Most-recent result bundle produced by this tab.
    pub const fn results(&self) -> &ResultBundle {
        &self.results
    }

    /// Mutable access to the result bundle.
    pub fn results_mut(&mut self) -> &mut ResultBundle {
        &mut self.results
    }

    /// Per-tab editor search state (separate from the result pane search).
    pub const fn editor_search(&self) -> &EditorSearchState {
        &self.editor_search
    }

    /// Page size used by the next sidebar preview.
    pub const fn page_size(&self) -> usize {
        self.page_size
    }

    /// Active completion popup, if any.
    pub const fn completion(&self) -> Option<&CompletionState> {
        self.completion.as_ref()
    }

    /// L36: read-only access to the staged-mutation queue.
    pub const fn pending(&self) -> &PendingChanges {
        &self.pending
    }

    /// L36: mutable handle to the staged-mutation queue. Used by
    /// tests and any future inline-edit path that needs to populate
    /// values on an `Insert` row without going through the cell
    /// editor.
    pub fn pending_mut(&mut self) -> &mut PendingChanges {
        &mut self.pending
    }

    /// L36: pending-preview modal state, if open.
    pub const fn pending_preview(&self) -> Option<&PendingPreviewState> {
        self.pending_preview.as_ref()
    }
}
