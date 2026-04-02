//! Editor context-menu key handler.
//!
//! Opened by a right-click inside the editor pane
//! (`core::dispatch::handle_right_click`); keyboard navigation
//! follows the modal-overlay convention: Up/Down + j/k cycle, Enter
//! accepts, Esc closes. While the menu is open every editor key is
//! routed here first (see `mode_basic.rs` / `mode_emacs.rs`).

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_vim::SearchDirection;

use crate::core::AppCore;
use crate::core::state::ui::ContextMenuAction;
use crate::run::RunMode;

impl AppCore {
    /// True if the editor context menu is currently open.
    #[must_use]
    pub const fn context_menu_open(&self) -> bool {
        self.ui.context_menu.is_some()
    }

    /// Handle one keystroke while the editor context menu is open.
    pub(crate) async fn handle_context_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.ui.context_menu.as_mut() else {
            return;
        };
        match key.code {
            CtKey::Esc => {
                self.ui.context_menu = None;
            }
            CtKey::Up | CtKey::Char('k') => {
                // Skip disabled entries on the way up.
                let len = menu.items.len();
                if len == 0 {
                    return;
                }
                let mut idx = menu.selected;
                for _ in 0..len {
                    idx = if idx == 0 { len - 1 } else { idx - 1 };
                    if !menu.items[idx].disabled {
                        break;
                    }
                }
                menu.selected = idx;
            }
            CtKey::Down | CtKey::Char('j') => {
                let len = menu.items.len();
                if len == 0 {
                    return;
                }
                let mut idx = menu.selected;
                for _ in 0..len {
                    idx = (idx + 1) % len;
                    if !menu.items[idx].disabled {
                        break;
                    }
                }
                menu.selected = idx;
            }
            CtKey::Enter | CtKey::Char(' ') => {
                let item = menu.items[menu.selected].clone();
                if item.disabled {
                    return;
                }
                self.ui.context_menu = None;
                self.run_context_menu_action(item.action).await;
            }
            _ => {}
        }
    }

    async fn run_context_menu_action(&mut self, action: ContextMenuAction) {
        match action {
            ContextMenuAction::Cut => self.context_menu_cut(),
            ContextMenuAction::Copy => self.context_menu_copy(),
            ContextMenuAction::Paste => self.context_menu_paste(),
            ContextMenuAction::SelectAll => self.context_menu_select_all(),
            ContextMenuAction::RunSelection => self.context_menu_run_selection().await,
            ContextMenuAction::Find => {
                self.open_editor_search(SearchDirection::Forward).await;
            }
            ContextMenuAction::ToggleComment => self.context_menu_toggle_comment(),
        }
    }

    fn context_menu_cut(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let text = buf.selected_text();
        if text.is_empty() {
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
            return;
        }
        let before = buf.snapshot();
        let _ = buf.delete_selection();
        buf.commit_undo_snapshot(before);
        self.ui.status.message = format!("cut {} char(s)", text.chars().count());
    }

    fn context_menu_copy(&mut self) {
        let text = self.ui.tabs[self.ui.active_tab].editor.selected_text();
        if text.is_empty() {
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
        } else {
            self.ui.status.message = format!("copied {} char(s)", text.chars().count());
        }
    }

    fn context_menu_paste(&mut self) {
        let text = match self.deps.clipboard.get_text() {
            Ok(t) => t,
            Err(e) => {
                self.ui.status.message = format!("clipboard error: {e}");
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        }
        buf.insert_str(&text);
        buf.commit_undo_snapshot(before);
    }

    fn context_menu_select_all(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let last_row = buf.line_count().saturating_sub(1);
        let last_col = buf.get_line(last_row).len();
        buf.set_selection(Some(narwhal_domain::editor::Selection::character(
            (0, 0),
            (last_row, last_col),
        )));
    }

    async fn context_menu_run_selection(&mut self) {
        // Run the current statement; the run loop already trims to
        // the selection when one is active, so this matches the
        // F5 / Alt-Enter UX.
        self.dispatch_current_statement(RunMode::Execute).await;
    }

    /// Toggle a `--` line comment on every line touched by the
    /// selection (or the current line when no selection).
    fn context_menu_toggle_comment(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let (start_row, end_row) = if let Some(sel) = buf.selection() {
            let (s, e) = sel.normalised();
            (s.0, e.0)
        } else {
            let r = buf.cursor_row();
            (r, r)
        };
        let before = buf.snapshot();
        // Decide on add vs remove: if every line already starts with
        // `-- ` we strip; otherwise we prepend.
        let all_commented = (start_row..=end_row).all(|r| {
            let line = buf.get_line(r).trim_start();
            line.starts_with("--")
        });
        for r in start_row..=end_row {
            let line = buf.get_line(r).to_owned();
            let new_line = if all_commented {
                // Strip leading `-- ` or `--`.
                let trimmed = line.trim_start();
                let indent = &line[..line.len() - trimmed.len()];
                let body = trimmed
                    .strip_prefix("-- ")
                    .or_else(|| trimmed.strip_prefix("--"))
                    .unwrap_or(trimmed);
                format!("{indent}{body}")
            } else {
                let trimmed = line.trim_start();
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}-- {trimmed}")
            };
            buf.replace_line(r, &new_line);
        }
        buf.commit_undo_snapshot(before);
    }
}
