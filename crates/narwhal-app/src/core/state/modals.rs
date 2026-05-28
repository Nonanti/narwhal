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

use narwhal_config::{EditorMode, KeyPreset, MouseSelectionMode, Settings, Theme};
use narwhal_core::Value;

use super::{GotoModal, HistoryState, SnippetsModal};
use crate::wizard::ConnectionWizard;

/// In-app settings editor.
///
/// Opened by `:settings`; modifies a `draft` copy of the live
/// settings so Esc cancels cleanly. `Ctrl+S` writes the draft to
/// disk and re-applies it to the running `AppCore`.
///
/// The modal is organised as a list of sections (Editor, Theme,
/// Display, Keybindings, Runtime). Within each section the fields
/// render as a vertical list; `Tab` switches sections,
/// `Up/Down/j/k` walk fields, `Space`/`Enter` toggles or cycles
/// the highlighted field's value.
#[derive(Debug, Clone)]
pub struct SettingsModal {
    /// Working copy mutated by the user. Committed by `Ctrl+S`,
    /// dropped by `Esc`.
    pub draft: Settings,
    /// Snapshot taken when the modal opened; restored on `Esc`
    /// without touching disk.
    pub original: Settings,
    pub selected_section: usize,
    pub selected_field: usize,
    /// `true` while a draft edit has not yet been persisted.
    pub dirty: bool,
    /// Sticky message shown in the modal footer.
    pub message: String,
}

impl SettingsModal {
    /// Build a modal whose draft is the current settings.
    #[must_use]
    pub fn new(current: Settings) -> Self {
        Self {
            draft: current.clone(),
            original: current,
            selected_section: 0,
            selected_field: 0,
            dirty: false,
            message: "Tab: section | Up/Down: field | Space: toggle | Ctrl+S: save | Esc: cancel"
                .into(),
        }
    }

    /// Mark the draft dirty and refresh the footer hint.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.message = "modified — press Ctrl+S to save, Esc to discard".into();
    }

    /// Cycle the editor mode to the next variant.
    pub fn cycle_editor_mode(&mut self) {
        self.draft.editor.mode = match self.draft.editor.mode {
            EditorMode::Vim => EditorMode::Basic,
            EditorMode::Basic => EditorMode::Emacs,
            _ => EditorMode::Vim,
        };
        self.mark_dirty();
    }

    /// Cycle the mouse mode to the next variant.
    pub fn cycle_mouse_mode(&mut self) {
        self.draft.editor.mouse = match self.draft.editor.mouse {
            MouseSelectionMode::Enabled => MouseSelectionMode::ClickOnly,
            MouseSelectionMode::ClickOnly => MouseSelectionMode::Disabled,
            _ => MouseSelectionMode::Enabled,
        };
        self.mark_dirty();
    }

    /// Cycle through the available themes.
    pub fn cycle_theme(&mut self) {
        self.draft.theme = match self.draft.theme {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::HighContrast,
            _ => Theme::Dark,
        };
        self.mark_dirty();
    }

    /// Cycle through the keybinding presets.
    pub fn cycle_preset(&mut self) {
        self.draft.keybindings.preset = match self.draft.keybindings.preset {
            KeyPreset::Default => KeyPreset::Vscode,
            KeyPreset::Vscode => KeyPreset::Datagrip,
            KeyPreset::Datagrip => KeyPreset::Intellij,
            _ => KeyPreset::Default,
        };
        self.mark_dirty();
    }
}

/// What the user is about to do once they confirm.
///
/// Bundled with [`ConfirmModal`] so the dispatcher can resume the
/// exact action that was held back without the modal needing a
/// reference to driver state. The modal layer only sees an opaque
/// `kind` and a `prompt`.
#[derive(Debug, Clone)]
pub enum PendingConfirm {
    /// the user pressed F5/F6 on a connection that declared
    /// `confirm_writes = true` and at least one statement classifies
    /// as a write or DDL. Holds the statement batch + mode so the
    /// run can proceed verbatim once confirmed.
    RunMutatingBatch {
        statements: Vec<String>,
        /// CR-1: per-statement bound parameters. Empty when the batch
        /// came from the interactive editor path (non-parametric);
        /// non-empty when the batch was queued via
        /// `dispatch_batch_with_params` (foreign-key navigation,
        /// programmatic templating, …). If we dropped this on the
        /// floor and resumed via the non-parametric path, the driver
        /// would see placeholders (`$1`, `?`) with no bindings.
        params_per_statement: Vec<Vec<Value>>,
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
    pub fn write_confirm(connection_name: &str, sample_sql: &str, action: PendingConfirm) -> Self {
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

    /// M-D: append `c` to the buffer unless the cap is reached.
    /// Silently drops further input to keep the modal responsive on
    /// an adversarial bracketed paste / wedged key. The cap is
    /// generous (256 chars) so any plausible accept keyword fits
    /// comfortably, but bounded so a paste cannot drive the buffer
    /// (and the re-rendered prompt) into the hundreds of MB.
    pub fn try_push(&mut self, c: char) {
        if self.buffer.len() < Self::BUFFER_CAP {
            self.buffer.push(c);
        }
    }

    /// Upper bound on [`Self::buffer`] growth — see [`Self::try_push`].
    pub const BUFFER_CAP: usize = 256;
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
    /// "type YES to run" confirmation. Opened by the
    /// write-guard before a mutating batch reaches the driver on a
    /// connection that opted in to `confirm_writes = true`.
    pub confirm: Option<ConfirmModal>,
    /// `:goto` fuzzy schema navigator. Owns the corpus
    /// snapshot at open time and the user's query state.
    pub goto: Option<GotoModal>,
    /// In-app settings editor (`:settings`). Holds the draft until
    /// Ctrl+S writes it to `settings.toml`.
    pub settings: Option<SettingsModal>,
}

impl ModalState {
    /// True if any keyboard-owning modal is open, excluding the
    /// editor context menu (which is anchored to the editor and
    /// handled separately). Used by the mouse handler to block
    /// background state mutations while a modal is in the foreground.
    pub const fn any_open_except_context_menu(&self) -> bool {
        self.any_open()
    }

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
            || self.settings.is_some()
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
        self.settings = None;
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
                params_per_statement: Vec::new(),
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
                params_per_statement: Vec::new(),
                stream: false,
            },
        );
        assert!(
            m.prompt.contains('\u{2026}'),
            "long line truncated with \u{2026}"
        );
    }

    #[test]
    fn pending_confirm_preserves_bound_parameters() {
        // CR-1 regression: dropping `params_per_statement` on the way
        // through the modal would mean a resumed FK navigation (or
        // any other parametric batch routed through dispatch_batch_
        // with_params) reached the driver without bindings.
        let action = PendingConfirm::RunMutatingBatch {
            statements: vec!["UPDATE u SET x = $1 WHERE id = $2".into()],
            params_per_statement: vec![vec![Value::Int(7), Value::Int(42)]],
            stream: false,
        };
        let PendingConfirm::RunMutatingBatch {
            params_per_statement,
            ..
        } = action;
        assert_eq!(params_per_statement.len(), 1);
        assert_eq!(params_per_statement[0].len(), 2);
        assert!(matches!(params_per_statement[0][0], Value::Int(7)));
        assert!(matches!(params_per_statement[0][1], Value::Int(42)));
    }

    #[test]
    fn try_push_respects_buffer_cap() {
        // M-D regression: an adversarial bracketed paste or a wedged
        // key would otherwise grow `buffer` without bound, and the
        // status-bar re-render formats the buffer on every redraw.
        let mut m = ConfirmModal::write_confirm(
            "prod",
            "DELETE FROM t",
            PendingConfirm::RunMutatingBatch {
                statements: vec!["DELETE FROM t".into()],
                params_per_statement: Vec::new(),
                stream: false,
            },
        );
        for _ in 0..(ConfirmModal::BUFFER_CAP + 32) {
            m.try_push('x');
        }
        assert_eq!(m.buffer.len(), ConfirmModal::BUFFER_CAP);
    }

    #[test]
    fn close_all_clears_confirm() {
        let mut modals = ModalState {
            confirm: Some(ConfirmModal::write_confirm(
                "prod",
                "DELETE FROM t",
                PendingConfirm::RunMutatingBatch {
                    statements: vec!["DELETE FROM t".into()],
                    params_per_statement: Vec::new(),
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
