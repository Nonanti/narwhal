//! Emacs-style editor key handler.
//!
//! Active when `[editor].mode = "emacs"`. Implements the classic
//! Ctrl- / Meta- chord set: C-a / C-e for line ends, C-f / C-b for
//! character motion, C-n / C-p for line motion, C-d for forward
//! delete, M-w / C-w for copy / cut, C-y for yank, C-k for
//! kill-line, C-s for search, C-/ for undo, plus the two-stroke
//! `C-x` prefix (C-x C-s = run / submit, C-x u = undo).
//!
//! `C-Space` toggles the mark; subsequent motions extend the
//! selection until the next non-motion chord clears it.
//!
//! Like the basic-mode handler, this lives next to but does not
//! delegate into `narwhal_vim::Vim`. The completion popup and
//! command palette are shared infrastructure and still work.

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_domain::Motion as DomainMotion;
use narwhal_domain::editor::{Selection, SelectionKind};
use narwhal_vim::SearchDirection;

use crate::core::AppCore;

impl AppCore {
    pub(crate) async fn handle_editor_key_emacs(&mut self, key: KeyEvent) {
        if self.ui.tabs[self.ui.active_tab].completion.is_some()
            && self.handle_completion_key(key).await
        {
            return;
        }

        // C-x pending prefix: the next chord completes a two-stroke
        // binding. Cleared unconditionally on the next keystroke so
        // unknown follow-ups bail out cleanly.
        if let Some(prefix) = self.ui.emacs_pending_prefix.take()
            && prefix == 'x'
        {
            self.emacs_handle_cx_prefix(key).await;
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // Mark active <=> selection present. Motions extend it.
        let keep_selection = self.ui.tabs[self.ui.active_tab]
            .editor
            .selection()
            .is_some();

        match key.code {
            // ---------- C-x prefix ----------------------------
            CtKey::Char('x') if ctrl => {
                self.ui.emacs_pending_prefix = Some('x');
                self.ui.status.message = "C-x ...".into();
            }

            // ---------- character motions ---------------------
            CtKey::Char('f') if ctrl => {
                self.emacs_motion(DomainMotion::Right, keep_selection);
            }
            CtKey::Char('b') if ctrl => {
                self.emacs_motion(DomainMotion::Left, keep_selection);
            }
            CtKey::Char('n') if ctrl => {
                self.emacs_motion(DomainMotion::Down, keep_selection);
            }
            CtKey::Char('p') if ctrl => {
                self.emacs_motion(DomainMotion::Up, keep_selection);
            }
            CtKey::Char('a') if ctrl => {
                self.emacs_motion(DomainMotion::LineStart, keep_selection);
            }
            CtKey::Char('e') if ctrl => {
                self.emacs_motion(DomainMotion::LineEnd, keep_selection);
            }
            CtKey::Char('f') if alt => {
                self.emacs_motion(DomainMotion::WordForward, keep_selection);
            }
            CtKey::Char('b') if alt => {
                self.emacs_motion(DomainMotion::WordBackward, keep_selection);
            }
            CtKey::Char('<') if alt => {
                self.emacs_motion(DomainMotion::FileStart, keep_selection);
            }
            CtKey::Char('>') if alt => {
                self.emacs_motion(DomainMotion::FileEnd, keep_selection);
            }
            CtKey::Left => self.emacs_motion(DomainMotion::Left, keep_selection || shift),
            CtKey::Right => self.emacs_motion(DomainMotion::Right, keep_selection || shift),
            CtKey::Up => self.emacs_motion(DomainMotion::Up, keep_selection || shift),
            CtKey::Down => self.emacs_motion(DomainMotion::Down, keep_selection || shift),
            CtKey::Home => self.emacs_motion(DomainMotion::LineStart, keep_selection || shift),
            CtKey::End => self.emacs_motion(DomainMotion::LineEnd, keep_selection || shift),

            // ---------- mark / cancel -------------------------
            CtKey::Char(' ') if ctrl => {
                self.emacs_set_mark();
            }
            CtKey::Char('g') if ctrl => {
                self.ui.tabs[self.ui.active_tab].editor.clear_selection();
                self.ui.tabs[self.ui.active_tab].completion = None;
                self.ui.context_menu = None;
                self.ui.status.message = "quit".into();
            }
            CtKey::Esc => {
                self.ui.tabs[self.ui.active_tab].editor.clear_selection();
                self.ui.tabs[self.ui.active_tab].completion = None;
            }

            // ---------- editing -------------------------------
            CtKey::Char('d') if ctrl => self.emacs_delete_forward(),
            CtKey::Char('d') if alt => self.emacs_delete_word_forward(),
            CtKey::Char('k') if ctrl => self.emacs_kill_line(),
            CtKey::Backspace => self.emacs_backspace(),
            CtKey::Delete => self.emacs_delete_forward(),
            CtKey::Enter => self.emacs_insert_str("\n"),
            CtKey::Tab => {
                let prefix = self.ui.tabs[self.ui.active_tab]
                    .editor
                    .current_word_prefix();
                if prefix.is_empty() {
                    self.emacs_insert_str("    ");
                } else {
                    self.trigger_completion().await;
                }
            }

            // ---------- clipboard / undo ----------------------
            CtKey::Char('w') if ctrl => self.emacs_cut(),
            CtKey::Char('w') if alt => self.emacs_copy(),
            CtKey::Char('y') if ctrl => self.emacs_yank(),
            CtKey::Char('/') if ctrl => self.emacs_undo(),
            CtKey::Char('_') if ctrl => self.emacs_undo(),

            // ---------- search --------------------------------
            CtKey::Char('s') if ctrl => {
                self.open_editor_search(SearchDirection::Forward).await;
            }
            CtKey::Char('r') if ctrl => {
                self.open_editor_search(SearchDirection::Backward).await;
            }

            // ---------- command palette ----------------------
            CtKey::Char(':') if !ctrl && !alt => {
                self.emacs_open_command_prompt().await;
            }

            // ---------- plain typing -------------------------
            CtKey::Char(c) if !ctrl && !alt => {
                let mut s = [0_u8; 4];
                let encoded = c.encode_utf8(&mut s).to_owned();
                self.emacs_insert_str(&encoded);
            }

            _ => {}
        }

        self.maybe_auto_complete().await;
    }

    async fn emacs_handle_cx_prefix(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // C-x C-s: submit / run the current statement (the
            // emacs muscle memory for "save").
            CtKey::Char('s') if ctrl => {
                self.dispatch_current_statement(crate::run::RunMode::Execute)
                    .await;
            }
            // C-x u: undo (alternative spelling).
            CtKey::Char('u') => self.emacs_undo(),
            _ => {
                self.ui.status.message = "C-x: unbound".into();
            }
        }
    }

    fn emacs_motion(&mut self, motion: DomainMotion, keep_selection: bool) {
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

    fn emacs_set_mark(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        buf.begin_or_extend_selection(SelectionKind::Character);
        self.ui.status.message = "mark set".into();
    }

    fn emacs_insert_str(&mut self, s: &str) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        }
        buf.insert_str(s);
        buf.commit_undo_snapshot(before);
    }

    fn emacs_backspace(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        if buf.has_selection() {
            let _ = buf.delete_selection();
        } else {
            buf.delete_char();
        }
        buf.commit_undo_snapshot(before);
    }

    fn emacs_delete_forward(&mut self) {
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

    fn emacs_delete_word_forward(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        buf.clear_selection();
        buf.begin_or_extend_selection(SelectionKind::Character);
        buf.apply_motion(DomainMotion::WordForward, 1);
        buf.begin_or_extend_selection(SelectionKind::Character);
        let killed = buf.delete_selection();
        if !killed.is_empty() {
            let _ = self.deps.clipboard.set_text(&killed);
        }
        buf.commit_undo_snapshot(before);
    }

    /// `C-k`: kill from cursor to end of line; if cursor is already
    /// at EOL, join the next line in (classic emacs behaviour).
    fn emacs_kill_line(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let before = buf.snapshot();
        buf.clear_selection();
        let row = buf.cursor_row();
        let col = buf.cursor_col();
        let line_len = buf.get_line(row).len();
        let target = if col >= line_len {
            // Kill into the next line (join).
            (row + 1, 0)
        } else {
            (row, line_len)
        };
        buf.set_selection(Some(Selection::character((row, col), target)));
        let killed = buf.delete_selection();
        if !killed.is_empty() {
            let _ = self.deps.clipboard.set_text(&killed);
        }
        buf.commit_undo_snapshot(before);
    }

    fn emacs_copy(&mut self) {
        let text = self.ui.tabs[self.ui.active_tab].editor.selected_text();
        if text.is_empty() {
            self.ui.status.message = "mark not set".into();
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
        } else {
            self.ui.status.message = "copied region".into();
        }
        self.ui.tabs[self.ui.active_tab].editor.clear_selection();
    }

    fn emacs_cut(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        let text = buf.selected_text();
        if text.is_empty() {
            self.ui.status.message = "mark not set".into();
            return;
        }
        if let Err(e) = self.deps.clipboard.set_text(&text) {
            self.ui.status.message = format!("clipboard error: {e}");
            return;
        }
        let before = buf.snapshot();
        let _ = buf.delete_selection();
        buf.commit_undo_snapshot(before);
        self.ui.status.message = "killed region".into();
    }

    fn emacs_yank(&mut self) {
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
        self.emacs_insert_str(&text);
    }

    fn emacs_undo(&mut self) {
        let buf = &mut self.ui.tabs[self.ui.active_tab].editor;
        if buf.undo() {
            self.ui.status.message = "undo".into();
        } else {
            self.ui.status.message = "nothing to undo".into();
        }
    }

    async fn emacs_open_command_prompt(&mut self) {
        let key = KeyEvent::new(CtKey::Char(':'), KeyModifiers::NONE);
        let Some(logical) = narwhal_tui::translate_key_event(key) else {
            return;
        };
        let action = self.ui.vim.handle(logical);
        self.apply_action(action).await;
    }
}
