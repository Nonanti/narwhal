//! Editor / sidebar / completion / search dispatchers split out of
//! `core::editor_dispatch`. The top-level `handle_global_key`
//! dispatcher lives here; sub-modules hold the actual per-pane
//! implementations.

mod completion;
mod context_menu;
mod editor_keys;
mod mode_basic;
mod mode_emacs;
mod search;
mod settings_modal;
mod sidebar;

pub(crate) use settings_modal::SECTION_LABELS;

use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_tui::Pane;
use narwhal_vim::Mode;

use crate::core::AppCore;
use crate::run::RunMode;

impl AppCore {
    pub(crate) async fn handle_global_key(&mut self, key: KeyEvent) -> bool {
        // Terminal-agnostic function keys first. Most terminal emulators
        // forward F-keys and Alt-Enter as distinct events, while Ctrl +
        // punctuation (Ctrl-;, Ctrl-/) is frequently swallowed by the
        // VT100-style key encoding before it ever reaches the program.
        match key.code {
            CtKey::F(1) => {
                self.toggle_help().await;
                return true;
            }
            CtKey::F(5) => {
                self.dispatch_current_statement(RunMode::Execute).await;
                return true;
            }
            CtKey::F(6) => {
                self.dispatch_all_statements(RunMode::Execute).await;
                return true;
            }
            CtKey::F(7) => {
                self.dispatch_current_statement(RunMode::Stream).await;
                return true;
            }
            CtKey::F(4) if self.process.running => {
                self.spawn_cancel();
                return true;
            }
            CtKey::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.dispatch_current_statement(RunMode::Execute).await;
                return true;
            }
            _ => {}
        }
        // In basic / emacs modes the editor reclaims a handful of
        // chords that the global layer otherwise eats (focus cycle,
        // goto, history, stream, completion). Without this short-
        // circuit, `C-w` would cycle focus instead of killing the
        // region in emacs, `C-n` would open the goto modal instead
        // of moving down a line, etc.
        if self.ui.focus == Pane::Editor
            && self.ui.editor_mode != narwhal_config::EditorMode::Vim
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, CtKey::Char('w' | 'n' | 'r' | 's' | ' '))
        {
            return false;
        }

        // Keybinding preset extras: each preset binds a small set of
        // discoverable IDE chords on top of the built-in defaults.
        // VSCode: Ctrl+P opens goto, Ctrl+Shift+P opens the command
        // palette equivalent.
        // DataGrip / IntelliJ: Ctrl+B focuses the sidebar,
        // Ctrl+Enter runs the statement under the cursor.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match (self.ui.key_preset, key.code) {
                (narwhal_config::KeyPreset::Vscode, CtKey::Char('p'))
                    if !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if !self.modals.any_open() {
                        self.open_goto_modal().await;
                        return true;
                    }
                }
                (narwhal_config::KeyPreset::Vscode, CtKey::Char('p'))
                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if self.ui.focus != Pane::Editor {
                        self.ui.focus = Pane::Editor;
                    }
                    let k = crossterm::event::KeyEvent::new(CtKey::Char(':'), KeyModifiers::NONE);
                    self.handle_editor_key(k).await;
                    return true;
                }
                (
                    narwhal_config::KeyPreset::Datagrip | narwhal_config::KeyPreset::Intellij,
                    CtKey::Char('b'),
                ) => {
                    self.ui.focus = Pane::Sidebar;
                    self.ui.status.message = format!("focus → {}", Pane::Sidebar.label());
                    return true;
                }
                (
                    narwhal_config::KeyPreset::Datagrip | narwhal_config::KeyPreset::Intellij,
                    CtKey::Enter,
                ) => {
                    self.dispatch_current_statement(RunMode::Execute).await;
                    return true;
                }
                _ => {}
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                CtKey::Char('w') => {
                    // Shift+Ctrl+W cycles backwards (L27).
                    self.ui.focus = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.ui.focus.cycle_back()
                    } else {
                        self.ui.focus.cycle()
                    };
                    self.ui.status.message = format!("focus → {}", self.ui.focus.label());
                    return true;
                }
                CtKey::Char('c') if self.process.running => {
                    self.spawn_cancel();
                    return true;
                }
                CtKey::Char(';') => {
                    self.dispatch_current_statement(RunMode::Execute).await;
                    return true;
                }
                CtKey::Char(' ')
                    if self.ui.focus == Pane::Editor && self.ui.vim.mode() == Mode::Insert =>
                {
                    // Ctrl-Space is the IDE-standard completion trigger
                    // and survives most terminal key-encoding layers.
                    // Only fires when the editor pane is focused and
                    // we're in insert mode — in normal mode it would
                    // collide with the vim layer's leader.
                    self.trigger_completion().await;
                    return true;
                }
                // L36: Ctrl-S is now reserved for the Results pane's
                // "commit pending" action. Streaming still has F7 and
                // `:stream`, so we drop the global binding here rather
                // than overload the chord with two meanings. Without
                // this, the global handler would short-circuit before
                // the pending-commit action ever ran.
                CtKey::Char('s') if self.ui.focus == Pane::Editor => {
                    self.dispatch_current_statement(RunMode::Stream).await;
                    return true;
                }
                CtKey::Tab => {
                    self.cycle_tab(1).await;
                    return true;
                }
                CtKey::BackTab => {
                    self.cycle_tab(-1).await;
                    return true;
                }
                CtKey::Char('t') => {
                    self.new_tab().await;
                    return true;
                }
                CtKey::Char('r') => {
                    self.open_history().await;
                    return true;
                }
                // v1.1 #1: Ctrl-N opens the goto fuzzy navigator from
                // any focus. Mirrors the DataGrip / IntelliJ binding.
                //
                // M1: defer to the editor's completion popup when it's
                // open and the editor pane is focused. Vim and most
                // IDE-style editors bind Ctrl-N to "next completion";
                // intercepting it while the popup is visible would
                // strand the user with no way to advance the list.
                //
                // MI-2: also defer when ANY other modal is already
                // open (history search, snippet picker, wizard,
                // confirm "type YES", goto itself, help overlay).
                // Otherwise Ctrl-N would stack a goto modal on top
                // of, say, the confirm-writes prompt.
                CtKey::Char('n') => {
                    if self.ui.focus == Pane::Editor
                        && self.ui.tabs[self.ui.active_tab].completion.is_some()
                    {
                        return false;
                    }
                    if self.modals.any_open() {
                        return false;
                    }
                    self.open_goto_modal().await;
                    return true;
                }
                CtKey::PageDown => {
                    self.cycle_result_tab(1).await;
                    return true;
                }
                CtKey::PageUp => {
                    self.cycle_result_tab(-1).await;
                    return true;
                }
                _ => {}
            }
        }
        // ? opens help in normal mode when the editor pane is NOT focused.
        // In the editor pane, ? is reserved for reverse search (plan 06-06).
        if key.code == CtKey::Char('?')
            && key.modifiers.is_empty()
            && self.ui.vim.mode() == Mode::Normal
            && self.ui.focus != Pane::Editor
        {
            self.toggle_help().await;
            return true;
        }
        // `:` opens the command palette from any non-editor pane.
        // Without this, users focused on the sidebar/results would have to
        // press Ctrl-W back to the editor before being able to type
        // `:open <conn>`. We snap focus to the editor and forward the
        // keystroke so the vim layer enters Command mode normally.
        if key.code == CtKey::Char(':') && key.modifiers.is_empty() && self.ui.focus != Pane::Editor
        {
            self.ui.focus = Pane::Editor;
            self.handle_editor_key(key).await;
            return true;
        }
        false
    }
}
