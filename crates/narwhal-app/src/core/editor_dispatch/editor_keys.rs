//! Editor pane key handling and action interpretation.

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_core::ColumnHeader;
use narwhal_domain::Motion as DomainMotion;
use narwhal_tui::translate_key_event;
use narwhal_vim::{Action, Mode, Motion as VimMotion, Operator};

use crate::completion::{detect_context_with_schemas, gather as gather_completions};
use crate::core::{AppCore, CompletionState};

/// Convert a `narwhal_vim::Motion` to `narwhal_domain::Motion`.
///
/// The two enums are isomorphic but live in separate crates to avoid
/// a domain-level dependency on the vim crate.
const fn domain_motion(m: VimMotion) -> DomainMotion {
    match m {
        VimMotion::Left => DomainMotion::Left,
        VimMotion::Right => DomainMotion::Right,
        VimMotion::Up => DomainMotion::Up,
        VimMotion::Down => DomainMotion::Down,
        VimMotion::WordForward => DomainMotion::WordForward,
        VimMotion::WordBackward => DomainMotion::WordBackward,
        VimMotion::LineStart => DomainMotion::LineStart,
        VimMotion::LineEnd => DomainMotion::LineEnd,
        VimMotion::FileStart => DomainMotion::FileStart,
        VimMotion::FileEnd => DomainMotion::FileEnd,
        VimMotion::CurrentLine => DomainMotion::CurrentLine,
        // `narwhal_vim::Motion` is #[non_exhaustive]; future variants
        // map to a no-op motion.
        _ => DomainMotion::CurrentLine,
    }
}

impl AppCore {
    pub(crate) fn accept_completion_at(&mut self, index: usize) {
        let Some(state) = self.tabs[self.active_tab].completion.as_mut() else {
            return;
        };
        if index >= state.items.len() {
            return;
        }
        state.selected = index;
        let choice = state.items[index].text.clone();
        self.tabs[self.active_tab]
            .editor
            .replace_current_word_with(&choice);
        self.tabs[self.active_tab].completion = None;
        self.status.message = format!("completed: {choice}");
    }

    /// Click on a sidebar table row: navigate the sidebar to that index
    /// and run a preview query. Uses `run_preview` (same as the
    /// keyboard-driven `o` path) so that `pending_source` is set and
    /// cell editing (`e`) works on mouse-previewed tables (M15).
    pub(crate) fn click_result_tab(&mut self, result_idx: usize) {
        let bundle = &mut self.tabs[self.active_tab].results;
        if result_idx < bundle.len() && bundle.is_multi() {
            bundle.active = result_idx;
            let total = bundle.len();
            self.status.message = format!("result {} of {total}", result_idx + 1);
        }
    }

    pub(crate) fn handle_editor_key(&mut self, key: KeyEvent) {
        // The editor search prompt is modal: characters build the needle,
        // Enter accepts, Esc cancels and restores the cursor.
        if self.tabs[self.active_tab].editor_search.prompt_open {
            self.handle_editor_search_key(key);
            return;
        }
        // The completion popup is modal while it's open: Tab cycles,
        // Enter accepts, Esc closes. Plain character keys fall through
        // so the user can keep typing and the popup refreshes against
        // the new prefix on the way out.
        if self.tabs[self.active_tab].completion.is_some() && self.handle_completion_key(key) {
            return;
        }
        // In insert mode, intercept a plain Tab so it triggers completion
        // instead of being forwarded to the vim layer.
        if self.vim.mode() == Mode::Insert && key.code == CtKey::Tab && key.modifiers.is_empty() {
            self.trigger_completion();
            return;
        }
        let Some(logical) = translate_key_event(key) else {
            return;
        };
        let action = self.vim.handle(logical);
        self.apply_action(action);

        // After every insert-mode keystroke, refresh the completion
        // popup against the new word prefix. Two thresholds:
        // - prefix.len() >= 2 opens or refreshes the popup;
        // - prefix.len() < 2 closes any open popup so the user can
        //   type short words without a flashing list.
        // Silent: no status spam, no '4-space' fallback — manual Tab
        // / Ctrl-Space still handle those cases.
        if self.vim.mode() == Mode::Insert {
            self.maybe_auto_complete();
        }
    }

    /// Build a column-name lookup map from the session's schema cache.
    ///
    /// Keys are lowercased table names; values are `(schema_name, columns)`
    /// tuples so each column completion can carry the schema as its detail
    /// string. Returns an empty map when no session is active.
    pub(crate) fn column_cache(
        &self,
    ) -> std::collections::HashMap<String, (String, Vec<ColumnHeader>)> {
        let Some(session) = self.session.active.as_ref() else {
            return std::collections::HashMap::new();
        };
        let mut map = std::collections::HashMap::new();
        for (schema, tables) in &session.schemas {
            for table in tables {
                let key = table.name.to_ascii_lowercase();
                // Only insert if not already present (first schema wins).
                map.entry(key)
                    .or_insert_with(|| (schema.name.clone(), Vec::new()));
            }
        }
        // Merge any cached column data from the session.
        for (table_lower, (schema_name, cols)) in &session.column_cache {
            map.insert(table_lower.clone(), (schema_name.clone(), cols.clone()));
        }
        map
    }

    /// Refresh-or-close the completion popup based on the current word
    /// prefix. Called after every insert-mode keystroke. See
    /// [`Self::trigger_completion`] for the manual (Tab / Ctrl-Space)
    /// variant that handles the empty-prefix and no-matches cases
    /// explicitly.
    pub(crate) fn maybe_auto_complete(&mut self) {
        let prefix = self.tabs[self.active_tab].editor.current_word_prefix();
        if prefix.len() < 2 {
            self.tabs[self.active_tab].completion = None;
            return;
        }
        let schemas = self
            .session
            .active
            .as_ref()
            .map_or(&[][..], |s| s.schemas.as_slice());
        let known_schemas: Vec<String> = schemas.iter().map(|(s, _)| s.name.clone()).collect();
        let buffer_text = self.tabs[self.active_tab].editor.entire_text();
        let offset = self.tabs[self.active_tab].editor.cursor_byte_offset();
        let context = detect_context_with_schemas(&buffer_text, offset, &known_schemas);
        let columns = self.column_cache();
        let items = gather_completions(&prefix, schemas, &context, &columns, 50);
        if items.is_empty() {
            self.tabs[self.active_tab].completion = None;
            return;
        }
        // Preserve the user's current selection across keystrokes when
        // possible — a brand-new popup starts at index 0.
        let selected = self.tabs[self.active_tab]
            .completion
            .as_ref()
            .map_or(0, |c| c.selected.min(items.len() - 1));
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected,
            prefix,
        });
    }

    /// Open the editor search prompt (`/` for forward, `?` for backward).
    pub(crate) fn apply_action(&mut self, action: Action) {
        match action {
            Action::Move { motion, count } => {
                self.tabs[self.active_tab]
                    .editor
                    .apply_motion(domain_motion(motion), count);
            }
            Action::InsertText(text) => {
                self.tabs[self.active_tab].editor.insert_str(&text);
            }
            Action::DeleteChar => {
                self.tabs[self.active_tab].editor.delete_char();
            }
            Action::EnterMode(mode) => {
                self.status.message = match mode {
                    Mode::Insert => "-- INSERT --".into(),
                    Mode::Normal => "ready".into(),
                    Mode::Command => ":".into(),
                    Mode::Visual => "-- VISUAL --".into(),
                    Mode::VisualLine => "-- V-LINE --".into(),
                    Mode::OperatorPending(op) => format!(
                        "-- {} --",
                        match op {
                            Operator::Delete => "OPERATOR DELETE",
                            Operator::Yank => "OPERATOR YANK",
                            Operator::Change => "OPERATOR CHANGE",
                            // Future operators surface as a generic label.
                            _ => "OPERATOR",
                        }
                    ),
                    // Future modes default to a generic status line.
                    _ => "ready".into(),
                };
            }
            Action::SubmitCommand(cmd) => self.execute_command(&cmd),
            Action::Pending if self.vim.mode() == Mode::Command => {
                self.status.message = format!(":{}", self.vim.command_buffer());
            }
            Action::Pending => {}
            Action::PromptComplete => self.complete_prompt(),
            Action::OpenSearch(dir) => self.open_editor_search(dir),
            Action::RepeatSearch => self.repeat_editor_search(false),
            Action::RepeatSearchReverse => self.repeat_editor_search(true),
            Action::Operate { .. } => {}
            // Future Action variants are silently ignored until wired.
            _ => {}
        }
    }
}
