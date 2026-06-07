//! Modeless ("basic") editor key handler.
//!
//! Active when `[editor].mode = "basic"`. Every keystroke goes
//! through this dispatcher instead of the vim state machine. The
//! goal is the familiar IDE / GUI editor behaviour: arrow keys move
//! the cursor, Shift+arrow extends a selection, Ctrl+arrow jumps a
//! word, Ctrl+C/V/X/Z/Y do the clipboard and undo work, and any
//! plain character is inserted at the cursor.
//!
//! `:` still opens the command palette and `Ctrl+F` / `/` still
//! drive editor-search so users who switch to basic mode don't lose
//! access to the rest of the app.

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_domain::Motion as DomainMotion;
use narwhal_domain::editor::{Selection, SelectionKind};
use narwhal_vim::SearchDirection;

use crate::core::AppCore;

impl AppCore {
    /// Entry point for the basic editor mode dispatcher.
    pub(crate) async fn handle_editor_key_basic(&mut self, key: KeyEvent) {
        // Completion popup is modal while open — keep parity with
        // the vim path so the two modes share the same UX.
        if self.ui.tabs[self.ui.active_tab].completion.is_some()
            && self.handle_completion_key(key).await
        {
            return;
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Shift-held arrow keys extend the selection; bare arrows
        // drop any current selection.
        let keep_selection = shift;

        match key.code {
            // ---------- navigation ------------------------------
            CtKey::Left if ctrl => self.basic_motion(DomainMotion::WordBackward, keep_selection),
            CtKey::Right if ctrl => self.basic_motion(DomainMotion::WordForward, keep_selection),
            CtKey::Left => self.basic_motion(DomainMotion::Left, keep_selection),
            CtKey::Right => self.basic_motion(DomainMotion::Right, keep_selection),
            CtKey::Up => self.basic_motion(DomainMotion::Up, keep_selection),
            CtKey::Down => self.basic_motion(DomainMotion::Down, keep_selection),
            CtKey::Home if ctrl => self.basic_motion(DomainMotion::FileStart, keep_selection),
            CtKey::End if ctrl => self.basic_motion(DomainMotion::FileEnd, keep_selection),
            CtKey::Home => self.basic_motion(DomainMotion::LineStart, keep_selection),
            CtKey::End => self.basic_motion(DomainMotion::LineEnd, keep_selection),
            CtKey::PageUp => {
                for _ in 0..10 {
                    self.basic_motion(DomainMotion::Up, keep_selection);
                }
            }
            CtKey::PageDown => {
                for _ in 0..10 {
                    self.basic_motion(DomainMotion::Down, keep_selection);
                }
            }

            // ---------- editing ---------------------------------
            CtKey::Backspace => self.basic_backspace(),
            CtKey::Delete => self.basic_delete_forward(),
            CtKey::Enter => self.basic_insert_str("\n"),
            CtKey::Tab => {
                // Inside a word, surface completion (same UX as vim
                // insert mode). Otherwise insert four spaces.
                let prefix = self.ui.tabs[self.ui.active_tab]
                    .editor
                    .current_word_prefix();
                if prefix.is_empty() {
                    self.basic_insert_str("    ");
                } else {
                    self.trigger_completion().await;
                }
            }

            // ---------- clipboard & undo -----------------------
            CtKey::Char('c') if ctrl => self.basic_copy(),
            CtKey::Char('x') if ctrl => self.basic_cut(),
            CtKey::Char('v') if ctrl => self.basic_paste(),
            CtKey::Char('a') if ctrl => self.basic_select_all(),
            CtKey::Char('z') if ctrl && shift => self.basic_redo(),
            CtKey::Char('z') if ctrl => self.basic_undo(),
            CtKey::Char('y') if ctrl => self.basic_redo(),

            // ---------- search ---------------------------------
            CtKey::Char('f') if ctrl => {
                self.open_editor_search(SearchDirection::Forward).await;
            }
            CtKey::Char('/') if !ctrl && !alt => {
                self.open_editor_search(SearchDirection::Forward).await;
            }

            // ---------- command palette ------------------------
            CtKey::Char(':') if !ctrl && !alt => {
                self.basic_open_command_prompt().await;
            }

            // ---------- selection / cancel ---------------------
            CtKey::Esc => {
                self.ui.tabs[self.ui.active_tab].editor.clear_selection();
                self.ui.tabs[self.ui.active_tab].completion = None;
                self.ui.context_menu = None;
            }

            // ---------- plain typing ---------------------------
            CtKey::Char(c) if !ctrl && !alt => {
                let mut s = [0_u8; 4];
                let encoded = c.encode_utf8(&mut s).to_owned();
                self.basic_insert_str(&encoded);
            }

            _ => {}
        }

        // Auto-completion refresh, same threshold as vim insert mode.
        self.maybe_auto_complete().await;
    }

    /// Apply a motion. When `keep_selection` is true the selection
    /// is grown to the new cursor; otherwise it is cleared.
    fn basic_motion(&mut self, motion: DomainMotion, keep_selection: bool) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        if keep_selection {
            buf.begin_or_extend_selection(SelectionKind::Character);
        }
        buf.apply_motion(motion, 1);
        if keep_selection {
            buf.begin_or_extend_selection(SelectionKind::Character);
        } else {
            buf.clear_selection();
        }
    }

    fn basic_insert_str(&mut self, s: &str) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        }
        buf.insert_str(s);
        buf.commit_undo_snapshot(before);
    }

    fn basic_backspace(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        } else {
            buf.delete_char();
        }
        buf.commit_undo_snapshot(before);
    }

    fn basic_delete_forward(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        } else {
            buf.apply_motion(DomainMotion::Right, 1);
            buf.delete_char();
        }
        buf.commit_undo_snapshot(before);
    }

    fn basic_copy(&mut self) {
        let text = self.ui.tabs[self.ui.active_tab].editor.selected_text();
        if text.is_empty() {
            self.ui.status.message = "nothing selected".into();
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
        } else {
            let chars = text.chars().count();
            self.ui.status.message = format!("copied {chars} char(s)");
        }
    }

    fn basic_cut(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let text = buf.selected_text();
        if text.is_empty() {
            self.ui.status.message = "nothing selected".into();
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
            return;
        }
        let before = buf.snapshot();
        let _ = buf.delete_selection();
        buf.commit_undo_snapshot(before);
        let chars = text.chars().count();
        self.ui.status.message = format!("cut {chars} char(s)");
    }

    fn basic_paste(&mut self) {
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
        self.basic_insert_str(&text);
    }

    fn basic_select_all(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let last_row = buf.line_count().saturating_sub(1);
        let last_col = buf.get_line(last_row).len();
        buf.set_selection(Some(Selection::character((0, 0), (last_row, last_col))));
    }

    fn basic_undo(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        if buf.undo() {
            self.ui.status.message = "undo".into();
        } else {
            self.ui.status.message = "nothing to undo".into();
        }
    }

    fn basic_redo(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        if buf.redo() {
            self.ui.status.message = "redo".into();
        } else {
            self.ui.status.message = "nothing to redo".into();
        }
    }

    /// Open the vim command-line for a single shot. Lets basic-mode
    /// users still type `:open`, `:write`, etc. without leaving
    /// their preferred editor model. The user types `:cmd<Enter>`
    /// and the vim machinery handles the rest.
    async fn basic_open_command_prompt(&mut self) {
        let key = KeyEvent::new(CtKey::Char(':'), KeyModifiers::NONE);
        let Some(logical) = narwhal_tui::translate_key_event(key) else {
            return;
        };
        let action = self.ui.vim.handle(logical);
        self.apply_action(action).await;
    }
}
