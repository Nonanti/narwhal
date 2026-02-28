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

use super::{GotoModal, HistoryState, SnippetsModal};
use crate::wizard::ConnectionWizard;

/// What the user is about to do once they confirm.
///
/// Bundled with [`ConfirmModal`] so the dispatcher can resume the
/// exact action that was held back without the modal needing a
/// reference to driver state. The modal layer only sees an opaque
/// `kind` and a `prompt`.
#[derive(Debug, Clone)]
pub enum PendingConfirm {
    /// v1.1 #2: the user pressed F5/F6 on a connection that declared
    /// `confirm_writes = true` and at least one statement classifies
    /// as a write or DDL. Holds the statement batch + mode so the
    /// run can proceed verbatim once confirmed.
    RunMutatingBatch {
        statements: Vec<String>,
        /// `true` if the user originally invoked stream-mode
        /// (F7 / `:stream`), `false` for execute-mode (F5/F6 /
        /// `:run`). Carried as a bool so this module stays free of
        /// the `RunMode` import (state ↛ run).
        stream: bool,
    },
}

/// Generic Y/N confirmation overlay.
///
/// Type-`YES` style instead of a single keystroke so a stray Enter
/// can't run a `DELETE FROM users` on prod. The buffer accumulates
/// the user's literal keystrokes; a match against the expected
/// `accept_keyword` (case-insensitive) is required to fire `action`.
#[derive(Debug, Clone)]
pub struct ConfirmModal {
    pub prompt: String,
    pub accept_keyword: String,
    pub buffer: String,
    pub action: PendingConfirm,
}

impl ConfirmModal {
    /// Build the default "writes on `<conn>`" modal. Hard-codes
    /// `YES` as the accept token; localising is a v2 concern.
    #[must_use]
    pub fn write_confirm(
        connection_name: &str,
        sample_sql: &str,
        action: PendingConfirm,
    ) -> Self {
        // Show a one-line preview of the first statement so the
        // user can sanity-check what they're about to authorise.
        let preview = sample_sql.lines().next().map_or("", str::trim);
        let truncated = if preview.chars().count() > 80 {
            let cut: String = preview.chars().take(77).collect();
            format!("{cut}\u{2026}")
        } else {
            preview.to_owned()
        };
        Self {
            prompt: format!(
                "Mutating statement on '{connection_name}':\n  {truncated}\n\nType YES to run, Esc to cancel."
            ),
            accept_keyword: "YES".into(),
            buffer: String::new(),
            action,
        }
    }

    /// `true` when the user has typed the accept keyword exactly
    /// (case-insensitive, ignoring leading/trailing whitespace).
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.buffer
            .trim()
            .eq_ignore_ascii_case(&self.accept_keyword)
    }
}

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
    /// v1.1 #2: "type YES to run" confirmation. Opened by the
    /// write-guard before a mutating batch reaches the driver on a
    /// connection that opted in to `confirm_writes = true`.
    pub confirm: Option<ConfirmModal>,
    /// v1.1 #1: `:goto` fuzzy schema navigator. Owns the corpus
    /// snapshot at open time and the user's query state.
    pub goto: Option<GotoModal>,
}

impl ModalState {
    /// True if any modal currently owns the foreground. Callers use
    /// this to decide whether a global hotkey (e.g. tab cycling)
    /// should fire or be swallowed by the modal layer.
    pub const fn any_open(&self) -> bool {
        self.wizard.is_some()
            || self.history.is_some()
            || self.snippets.is_some()
            || self.help_open
            || self.confirm.is_some()
            || self.goto.is_some()
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
        self.confirm = None;
        self.goto = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_satisfied_only_on_exact_match() {
        let m = ConfirmModal::write_confirm(
            "prod",
            "DELETE FROM users",
            PendingConfirm::RunMutatingBatch {
                statements: vec!["DELETE FROM users".into()],
                stream: false,
            },
        );
        assert!(!m.is_satisfied());

        let mut m = m;
        m.buffer = "yes".into();
        assert!(m.is_satisfied(), "case-insensitive");

        m.buffer = " YES ".into();
        assert!(m.is_satisfied(), "trims whitespace");

        m.buffer = "YEs!".into();
        assert!(!m.is_satisfied(), "must be exact keyword");

        m.buffer = "Y".into();
        assert!(!m.is_satisfied(), "single letter not enough");
    }

    #[test]
    fn confirm_preview_truncates_long_first_line() {
        let long = "DELETE FROM users WHERE ".to_string() + &"x".repeat(200);
        let m = ConfirmModal::write_confirm(
            "prod",
            &long,
            PendingConfirm::RunMutatingBatch {
                statements: vec![long.clone()],
                stream: false,
            },
        );
        assert!(m.prompt.contains('\u{2026}'), "long line truncated with \u{2026}");
    }

    #[test]
    fn close_all_clears_confirm() {
        let mut modals = ModalState {
            confirm: Some(ConfirmModal::write_confirm(
                "prod",
                "DELETE FROM t",
                PendingConfirm::RunMutatingBatch {
                    statements: vec!["DELETE FROM t".into()],
                    stream: false,
                },
            )),
            ..Default::default()
        };
        assert!(modals.any_open());
        modals.close_all();
        assert!(!modals.any_open());
    }
}
