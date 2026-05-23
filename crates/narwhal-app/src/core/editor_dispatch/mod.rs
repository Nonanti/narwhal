//! Editor / sidebar / completion / search dispatchers split out of
//! `core::editor_dispatch`. The top-level `handle_global_key`
//! dispatcher lives here; sub-modules hold the actual per-pane
//! implementations.

mod completion;
mod editor_keys;
mod search;
mod sidebar;

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_core::ColumnHeader;
use narwhal_tui::{translate_key_event, Pane};
use narwhal_vim::{Action, Mode, Operator, SearchDirection};
use tracing::debug;

use crate::core::text_utils::{
    find_all, longest_common_prefix, replace_all, replace_first, row_col_to_offset,
};
use crate::core::{AppCore, CompletionState, ResultState, RowSource, SidebarItem};
use crate::completion::{detect_context_with_schemas, gather as gather_completions};
use crate::run::RunMode;

impl AppCore {
    pub(crate) fn handle_global_key(&mut self, key: KeyEvent) -> bool {
        // Terminal-agnostic function keys first. Most terminal emulators
        // forward F-keys and Alt-Enter as distinct events, while Ctrl +
        // punctuation (Ctrl-;, Ctrl-/) is frequently swallowed by the
        // VT100-style key encoding before it ever reaches the program.
        match key.code {
            CtKey::F(1) => {
                self.toggle_help();
                return true;
            }
            CtKey::F(5) => {
                self.dispatch_current_statement(RunMode::Execute);
                return true;
            }
            CtKey::F(6) => {
                self.dispatch_all_statements(RunMode::Execute);
                return true;
            }
            CtKey::F(7) => {
                self.dispatch_current_statement(RunMode::Stream);
                return true;
            }
            CtKey::F(4) if self.running => {
                self.spawn_cancel();
                return true;
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.dispatch_current_statement(RunMode::Execute);
                return true;
            }
            _ => {}
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                CtKey::Char('w') => {
                    // Shift+Ctrl+W cycles backwards (L27).
                    self.focus = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.focus.cycle_back()
                    } else {
                        self.focus.cycle()
                    };
                    self.status.message = format!("focus → {}", self.focus.label());
                    return true;
                }
                CtKey::Char('c') if self.running => {
                    self.spawn_cancel();
                    return true;
                }
                CtKey::Char(';') => {
                    self.dispatch_current_statement(RunMode::Execute);
                    return true;
                }
                CtKey::Char(' ')
                    if self.focus == Pane::Editor && self.vim.mode() == Mode::Insert =>
                {
                    // Ctrl-Space is the IDE-standard completion trigger
                    // and survives most terminal key-encoding layers.
                    // Only fires when the editor pane is focused and
                    // we're in insert mode — in normal mode it would
                    // collide with the vim layer's leader.
                    self.trigger_completion();
                    return true;
                }
                CtKey::Char('s') => {
                    self.dispatch_current_statement(RunMode::Stream);
                    return true;
                }
                CtKey::Tab => {
                    self.cycle_tab(1);
                    return true;
                }
                CtKey::BackTab => {
                    self.cycle_tab(-1);
                    return true;
                }
                CtKey::Char('t') => {
                    self.new_tab();
                    return true;
                }
                CtKey::Char('r') => {
                    self.open_history();
                    return true;
                }
                CtKey::PageDown => {
                    self.cycle_result_tab(1);
                    return true;
                }
                CtKey::PageUp => {
                    self.cycle_result_tab(-1);
                    return true;
                }
                _ => {}
            }
        }
        // ? opens help in normal mode when the editor pane is NOT focused.
        // In the editor pane, ? is reserved for reverse search (plan 06-06).
        if key.code == CtKey::Char('?')
            && key.modifiers.is_empty()
            && self.vim.mode() == Mode::Normal
            && self.focus != Pane::Editor
        {
            self.toggle_help();
            return true;
        }
        // `:` opens the command palette from any non-editor pane.
        // Without this, users focused on the sidebar/results would have to
        // press Ctrl-W back to the editor before being able to type
        // `:open <conn>`. We snap focus to the editor and forward the
        // keystroke so the vim layer enters Command mode normally.
        if key.code == CtKey::Char(':') && key.modifiers.is_empty() && self.focus != Pane::Editor {
            self.focus = Pane::Editor;
            self.handle_editor_key(key);
            return true;
        }
        false
    }

}
