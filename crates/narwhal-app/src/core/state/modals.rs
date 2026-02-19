//! Modal-overlay state.
//!
//! Every modal in narwhal (wizard, help overlay, history search,
//! snippets picker) is mutually exclusive with the others *for
//! input-routing purposes* — at most one modal owns keypresses at a
//! time, and `core::run_loop::handle_key` walks them in priority
//! order. Bundling them in a single sub-state makes the precedence
//! check explicit and enforces a single source of truth: every
//! modal-related field is here, nowhere else.
//!
//! Other sub-states (UI, session, process) hold zero modal fields by
//! convention. If a future feature needs cross-modal state, add it
//! here rather than scattering it back into `AppCore`.

use super::{HistoryState, SnippetsModal};
use crate::wizard::ConnectionWizard;

/// All modal-overlay state for the app. Each field is `Option<…>`
/// (or a `bool` for the toggleable help screen) because at-most-one
/// modal owns the foreground at any time.
#[derive(Default)]
pub struct ModalState {
    /// Connection wizard. `Some` while `:add`, `:edit`, or
    /// `:add-url` flow is open. Closed on commit, cancel, or Esc.
    pub wizard: Option<ConnectionWizard>,
    /// Inline error rendered under the wizard form (validation
    /// failure, keyring write failure on commit, …). Cleared
    /// whenever the wizard closes or the user edits a field.
    pub wizard_error: Option<String>,
    /// `:history` or Ctrl+R modal. Owns its own search box and
    /// filtered listing.
    pub history: Option<HistoryState>,
    /// `:snippets` modal. Drives the snippet picker independently
    /// of the editor's autocomplete popup.
    pub snippets: Option<SnippetsModal>,
    /// `F1` / `?` help overlay. Boolean because the overlay has no
    /// internal state — it just dims everything behind it.
    pub help_open: bool,
}

impl ModalState {
    /// True if any modal currently owns the foreground. Callers use
    /// this to decide whether a global hotkey (e.g. tab cycling)
    /// should fire or be swallowed by the modal layer.
    pub const fn any_open(&self) -> bool {
        self.wizard.is_some() || self.history.is_some() || self.snippets.is_some() || self.help_open
    }

    /// Close every modal and drop their state. Used by the global
    /// Esc handler and by destructive transitions (connection
    /// switch, quit) that should not leave a half-open wizard
    /// hanging.
    pub fn close_all(&mut self) {
        self.wizard = None;
        self.wizard_error = None;
        self.history = None;
        self.snippets = None;
        self.help_open = false;
    }
}
