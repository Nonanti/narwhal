//! Key handler for the in-app `:settings` modal.
//!
//! Layout the user sees:
//! - top row: section tabs (Editor / Theme / Display / Keybindings)
//! - left column: section list (also indicates selected row)
//! - main area: field list for the active section
//! - footer: shortcut hints
//!
//! Navigation:
//! - `Tab` / `Shift+Tab`: cycle sections
//! - `Up`/`Down`/`j`/`k`: cycle fields inside the section
//! - `Space` or `Enter`: toggle/cycle the highlighted field
//! - `Ctrl+S`: save to disk + apply
//! - `Esc`: discard draft and close

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};

use crate::core::AppCore;

/// Per-section field counts. Keeping them as a const table keeps the
/// dispatcher in lockstep with the renderer; both pull the same
/// numbers.
pub(crate) const SECTION_LABELS: &[&str] =
    &["Editor", "Theme", "Display", "Keybindings"];

/// Field counts in the same order as `SECTION_LABELS`.
pub(crate) const SECTION_FIELD_COUNTS: &[usize] = &[
    4, // Editor: mode, mouse, line_numbers, show_mode_indicator
    1, // Theme: theme
    3, // Display: auto_indent, highlight_current_line, word_wrap
    1, // Keybindings: preset
];

impl AppCore {
    pub(crate) async fn handle_settings_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modals.settings.as_mut() else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            CtKey::Esc => {
                self.cancel_settings_modal().await;
            }
            CtKey::Char('s') if ctrl => {
                self.commit_settings_modal().await;
            }
            CtKey::Tab if shift => {
                if modal.selected_section == 0 {
                    modal.selected_section = SECTION_LABELS.len() - 1;
                } else {
                    modal.selected_section -= 1;
                }
                modal.selected_field = 0;
            }
            CtKey::Tab => {
                modal.selected_section =
                    (modal.selected_section + 1) % SECTION_LABELS.len();
                modal.selected_field = 0;
            }
            CtKey::Down | CtKey::Char('j') => {
                let len = SECTION_FIELD_COUNTS[modal.selected_section];
                if len > 0 {
                    modal.selected_field = (modal.selected_field + 1) % len;
                }
            }
            CtKey::Up | CtKey::Char('k') => {
                let len = SECTION_FIELD_COUNTS[modal.selected_section];
                if len > 0 {
                    modal.selected_field = if modal.selected_field == 0 {
                        len - 1
                    } else {
                        modal.selected_field - 1
                    };
                }
            }
            CtKey::Char(' ') | CtKey::Enter => {
                self.toggle_settings_field();
            }
            _ => {}
        }
    }

    /// Toggle / cycle the highlighted field according to the
    /// (section, field) coordinates held by the modal.
    fn toggle_settings_field(&mut self) {
        let Some(modal) = self.modals.settings.as_mut() else {
            return;
        };
        match (modal.selected_section, modal.selected_field) {
            // ---------- Editor ----------
            (0, 0) => modal.cycle_editor_mode(),
            (0, 1) => modal.cycle_mouse_mode(),
            (0, 2) => {
                modal.draft.editor.line_numbers = !modal.draft.editor.line_numbers;
                modal.mark_dirty();
            }
            (0, 3) => {
                modal.draft.editor.show_mode_indicator =
                    !modal.draft.editor.show_mode_indicator;
                modal.mark_dirty();
            }
            // ---------- Theme ----------
            (1, 0) => modal.cycle_theme(),
            // ---------- Display ----------
            (2, 0) => {
                modal.draft.editor.auto_indent = !modal.draft.editor.auto_indent;
                modal.mark_dirty();
            }
            (2, 1) => {
                modal.draft.editor.highlight_current_line =
                    !modal.draft.editor.highlight_current_line;
                modal.mark_dirty();
            }
            (2, 2) => {
                modal.draft.editor.word_wrap = !modal.draft.editor.word_wrap;
                modal.mark_dirty();
            }
            // ---------- Keybindings ----------
            (3, 0) => modal.cycle_preset(),
            _ => {}
        }
    }
}
