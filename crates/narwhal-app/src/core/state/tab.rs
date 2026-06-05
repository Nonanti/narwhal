//! One editor tab: name, buffer, run state, results.

use narwhal_pivot::PivotConfig;
use narwhal_sql::treesitter::{HighlightSpan, Parser as TsParser};
use narwhal_tui::EditorBuffer;

use crate::core::chart::ChartConfig;

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
    /// T1-T3-A: per-tab tree-sitter parser. Lazily initialised on the
    /// first highlight request so a tab that never opens an editor
    /// (e.g. one used only via :run) pays nothing. `None` after
    /// construction; populated by [`Tab::sql_highlights`].
    pub(crate) ts_parser: Option<TsParser>,
    /// T1-T3-A: cached highlight spans for the current editor buffer.
    /// Refreshed when the editor's content changes (the dispatch
    /// layer compares revision counters before re-rendering).
    pub(crate) sql_highlights: Option<Vec<HighlightSpan>>,
    /// T1-T3-A: byte length of the buffer the cached spans were
    /// computed against. A mismatch with the current buffer length
    /// invalidates the cache.
    pub(crate) sql_highlights_buf_len: usize,
    /// T2-T4-C: sticky chart configuration. When `Some`, the result
    /// pane splits horizontally and the top half renders an inline
    /// ASCII chart derived from the active result. `None` means the
    /// chart pane is hidden (default).
    pub(crate) chart: Option<ChartConfig>,
    /// T2-T4-D: sticky pivot configuration. When `Some`, the result
    /// pane splits (chart on top, pivot middle, table bottom). `None`
    /// keeps the pivot pane hidden.
    pub(crate) pivot: Option<PivotConfig>,
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
            ts_parser: None,
            sql_highlights: None,
            sql_highlights_buf_len: 0,
            chart: None,
            pivot: None,
        }
    }

    /// T1-T3-A: refresh and return the tree-sitter highlight spans
    /// for the current editor buffer.
    ///
    /// Cache policy: spans are recomputed when the buffer length
    /// changes. Within-line same-length edits (overstrike a single
    /// character) leave stale spans visible for one render tick — a
    /// minor visual glitch resolved by the next length-changing
    /// keystroke. A precise revision counter on `EditorBuffer`
    /// would close the hole; deferred until the multi-cursor task
    /// (T2-T3-D) adds one anyway.
    ///
    /// The reparse path goes through `Parser::reparse`, which falls
    /// back to a from-scratch parse the first time it's called per
    /// tab. Subsequent calls reuse the cached `tree_sitter::Tree`
    /// when the dispatch layer threads `Edit`s through (a future
    /// follow-up; the editor doesn't surface byte-level edit events
    /// yet).
    ///
    /// Returns `None` if the grammar failed to load (logged once at
    /// startup); the editor degrades to plain text in that case.
    pub fn sql_highlights(&mut self) -> Option<&[HighlightSpan]> {
        let source = self.editor.entire_text();
        if self.sql_highlights.is_some() && self.sql_highlights_buf_len == source.len() {
            return self.sql_highlights.as_deref();
        }
        if self.ts_parser.is_none() {
            match TsParser::new() {
                Ok(p) => self.ts_parser = Some(p),
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "tree-sitter SQL parser unavailable; \
                                        falling back to plain-text editor rendering"
                    );
                    return None;
                }
            }
        }
        let parser = self.ts_parser.as_mut()?;
        match parser.reparse(&source) {
            Ok(tree) => {
                self.sql_highlights = Some(tree.highlights(&source));
                self.sql_highlights_buf_len = source.len();
            }
            Err(err) => {
                tracing::warn!(?err, "tree-sitter SQL reparse failed; keeping last cache");
                return self.sql_highlights.as_deref();
            }
        }
        self.sql_highlights.as_deref()
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
    pub const fn editor_mut(&mut self) -> &mut EditorBuffer {
        &mut self.editor
    }

    /// Most-recent result bundle produced by this tab.
    pub const fn results(&self) -> &ResultBundle {
        &self.results
    }

    /// Mutable access to the result bundle.
    pub const fn results_mut(&mut self) -> &mut ResultBundle {
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
    pub const fn pending_mut(&mut self) -> &mut PendingChanges {
        &mut self.pending
    }

    /// L36: pending-preview modal state, if open.
    pub const fn pending_preview(&self) -> Option<&PendingPreviewState> {
        self.pending_preview.as_ref()
    }
}
