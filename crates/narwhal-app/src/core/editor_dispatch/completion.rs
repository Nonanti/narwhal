//! Editor / prompt completion plumbing.

use crossterm::event::{KeyCode as CtKey, KeyEvent};

use crate::completion::{detect_context_with_schemas, gather as gather_completions};
use crate::core::text_utils::longest_common_prefix;
use crate::core::{AppCore, CompletionState};

impl AppCore {
    pub(crate) fn trigger_completion(&mut self) {
        let prefix = self.ui.tabs[self.ui.active_tab]
            .editor
            .current_word_prefix();
        if prefix.is_empty() {
            // Empty prefix: behave like a plain insert (4 spaces).
            self.ui.tabs[self.ui.active_tab].editor.insert_str("    ");
            return;
        }
        let schemas = self
            .session
            .active
            .as_ref()
            .map_or(&[][..], |s| s.schemas.as_slice());
        let known_schemas: Vec<String> = schemas.iter().map(|(s, _)| s.name.clone()).collect();
        let buffer_text = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        let offset = self.ui.tabs[self.ui.active_tab].editor.cursor_byte_offset();
        let context = detect_context_with_schemas(&buffer_text, offset, &known_schemas);
        let columns = self.column_cache();
        let items = gather_completions(&prefix, schemas, &context, &columns, 50);
        if items.is_empty() {
            self.ui.status.message = format!("no completions for '{prefix}'");
            return;
        }
        if items.len() == 1 {
            // Exactly one match: insert it without showing the popup.
            let only = items[0].text.clone();
            self.ui.tabs[self.ui.active_tab]
                .editor
                .replace_current_word_with(&only);
            self.ui.status.message = format!("completed: {only}");
            return;
        }
        self.ui.tabs[self.ui.active_tab].completion = Some(CompletionState {
            items,
            selected: 0,
            prefix,
        });
        self.ui.status.message = "completion: ↑↓ cycles · Tab/Enter accepts · Esc cancels".into();
    }

    /// Returns `true` when the key was consumed by the completion popup.
    ///
    /// Bindings inside the popup follow the IDE convention used by
    /// `IntelliJ` / `DataGrip` / VS Code so the muscle memory transfers:
    /// - Tab / Enter: accept the selected completion
    /// - ↑ / ↓: move the highlight
    /// - Shift-Tab: previous highlight (kept for keyboards without
    ///   arrow access in vim-aware terminal multiplexers)
    /// - Esc: dismiss the popup; the editor stays in insert mode and
    ///   the originally typed prefix is preserved
    pub(crate) fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.ui.tabs[self.ui.active_tab].completion.as_mut() else {
            return false;
        };
        match key.code {
            CtKey::Esc => {
                self.ui.tabs[self.ui.active_tab].completion = None;
                self.ui.status.message = "completion cancelled".into();
                true
            }
            CtKey::Enter | CtKey::Tab => {
                let choice = state.items[state.selected].text.clone();
                self.ui.tabs[self.ui.active_tab]
                    .editor
                    .replace_current_word_with(&choice);
                self.ui.tabs[self.ui.active_tab].completion = None;
                self.ui.status.message = format!("completed: {choice}");
                true
            }
            CtKey::BackTab | CtKey::Up => {
                let len = state.items.len();
                state.selected = (state.selected + len - 1) % len;
                true
            }
            CtKey::Down => {
                state.selected = (state.selected + 1) % state.items.len();
                true
            }
            // Any other key dismisses the popup and falls through to the
            // editor so the keystroke takes effect.
            _ => {
                self.ui.tabs[self.ui.active_tab].completion = None;
                false
            }
        }
    }

    pub(crate) fn complete_prompt(&mut self) {
        let buf = self.ui.vim.command_buffer().to_owned();
        let parts: Vec<&str> = buf.split_whitespace().collect();
        let head = parts.first().copied().unwrap_or("");

        // Identify which universe to complete from.
        let universe: Vec<String> = match head {
            "open" | "o" | "remove" | "rm" | "forget" => self
                .session
                .connections
                .connections
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            "help" | "h" => {
                let mut v: Vec<String> = crate::commands::BUILTIN_COMMAND_NAMES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                v.extend(
                    self.plugins
                        .catalogue()
                        .into_iter()
                        .map(|(_, cmd)| cmd.name),
                );
                v
            }
            "export" => vec!["csv".into(), "json".into(), "insert".into()],
            "save" | "load" | "rm-snippet" | "rmsnippet" => {
                self.session.snippet_store.list().unwrap_or_default()
            }
            _ => return,
        };

        // The token being completed is the last whitespace-separated word;
        // if the buffer ends with whitespace we are starting a fresh token
        // (empty prefix).
        let prefix = if buf.ends_with(char::is_whitespace) {
            String::new()
        } else {
            parts.last().copied().unwrap_or("").to_owned()
        };

        // When the prefix is the command head itself (user typed
        // `:open` with no trailing space), we are not completing an
        // argument yet — skip so the command head isn't replaced.
        if prefix == head && !buf.ends_with(char::is_whitespace) {
            return;
        }

        let matches: Vec<&str> = universe
            .iter()
            .filter(|name| name.to_lowercase().starts_with(&prefix.to_lowercase()))
            .map(String::as_str)
            .collect();

        match matches.as_slice() {
            [] => {
                self.ui.status.message = format!("no completions for {prefix:?}");
            }
            [only] => {
                self.ui.vim.replace_command_token(only);
                self.ui.status.message = format!(":{}", self.ui.vim.command_buffer());
            }
            many => {
                let lcp = longest_common_prefix(many);
                if lcp.len() > prefix.len() {
                    self.ui.vim.replace_command_token(&lcp);
                }
                self.ui.status.message = many.join(" ");
            }
        }
    }
}
