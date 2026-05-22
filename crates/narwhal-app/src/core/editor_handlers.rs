//! Editor pane, completion, search, substitute, and global key handling
//! extracted from `core.rs` (L21).
//!
//! Hosts every method that touches the editor [`EditorBuffer`] or the
//! `:`-prompt — i.e. everything that isn't a result-pane key, sidebar
//! navigation, or modal lifecycle.
use crossterm::event::{KeyCode as CtKey, KeyEvent, KeyModifiers};
use narwhal_core::ColumnHeader;
use narwhal_tui::{translate_key_event, Pane};
use narwhal_vim::{Action, Mode, Operator, SearchDirection};
use tracing::debug;

use super::text_utils::{
    find_all, longest_common_prefix, replace_all, replace_first, row_col_to_offset,
};
use super::{AppCore, CompletionState, ResultState, RowSource, SidebarItem};
use crate::completion::{detect_context_with_schemas, gather as gather_completions};
use crate::run::RunMode;

impl AppCore {
    pub(super) fn accept_completion_at(&mut self, index: usize) {
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
    pub(super) fn click_sidebar_table(&mut self, sidebar_idx: usize) {
        let Some(item) = self.sidebar_items.get(sidebar_idx).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            return;
        };
        self.sidebar_index = sidebar_idx;
        self.run_preview(&schema, &name, 0);
    }

    /// Click on a result tab: switch to that result index.
    pub(super) fn click_result_tab(&mut self, result_idx: usize) {
        let bundle = &mut self.tabs[self.active_tab].results;
        if result_idx < bundle.len() && bundle.is_multi() {
            bundle.active = result_idx;
            let total = bundle.len();
            self.status.message = format!("result {} of {total}", result_idx + 1);
        }
    }

    pub(super) fn handle_global_key(&mut self, key: KeyEvent) -> bool {
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

    pub(super) fn handle_editor_key(&mut self, key: KeyEvent) {
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
    fn column_cache(&self) -> std::collections::HashMap<String, (String, Vec<ColumnHeader>)> {
        let Some(session) = self.session.as_ref() else {
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
    fn maybe_auto_complete(&mut self) {
        let prefix = self.tabs[self.active_tab].editor.current_word_prefix();
        if prefix.len() < 2 {
            self.tabs[self.active_tab].completion = None;
            return;
        }
        let schemas = self
            .session
            .as_ref()
            .map(|s| s.schemas.as_slice())
            .unwrap_or(&[]);
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
            .map(|c| c.selected.min(items.len() - 1))
            .unwrap_or(0);
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected,
            prefix,
        });
    }

    // ----- editor search -----

    /// Open the editor search prompt (`/` for forward, `?` for backward).
    fn open_editor_search(&mut self, direction: SearchDirection) {
        let tab = &mut self.tabs[self.active_tab];
        tab.editor_search.saved_cursor = Some(tab.editor.cursor());
        tab.editor_search.direction = direction;
        tab.editor_search.prompt_open = true;
        tab.editor_search.needle.clear();
        tab.editor_search.matches.clear();
        tab.editor_search.current = None;
        let prompt_char = match direction {
            SearchDirection::Forward => '/',
            SearchDirection::Backward => '?',
            // Future search directions: default to forward prompt.
            _ => '/',
        };
        self.status.message = format!("{prompt_char}");
    }

    /// Handle a key event while the editor search prompt is open.
    fn handle_editor_search_key(&mut self, key: KeyEvent) {
        match key.code {
            CtKey::Esc => {
                let tab = &mut self.tabs[self.active_tab];
                if let Some((row, col)) = tab.editor_search.saved_cursor.take() {
                    tab.editor.set_cursor(row, col);
                }
                tab.editor_search.prompt_open = false;
                tab.editor_search.needle.clear();
                tab.editor_search.matches.clear();
                tab.editor_search.current = None;
                tab.editor_search.highlight = false;
                self.status.message = "search cancelled".into();
            }
            CtKey::Enter => {
                let tab = &mut self.tabs[self.active_tab];
                tab.editor_search.prompt_open = false;
                tab.editor_search.highlight = true;
                // Set current to whatever match the cursor is on.
                self.sync_editor_search_current();
                let count = self.tabs[self.active_tab].editor_search.matches.len();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                if count == 0 {
                    self.status.message = format!("/{needle} · no matches");
                } else {
                    let idx = self.tabs[self.active_tab]
                        .editor_search
                        .current
                        .map(|i| i + 1)
                        .unwrap_or(1);
                    self.status.message = format!("/{needle} · {idx}/{count}");
                }
            }
            CtKey::Backspace => {
                self.tabs[self.active_tab].editor_search.needle.pop();
                self.refresh_editor_search_matches();
                self.jump_to_editor_search_match();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                let prompt_char = match self.tabs[self.active_tab].editor_search.direction {
                    SearchDirection::Forward => '/',
                    SearchDirection::Backward => '?',
                    // Future search directions: default to forward prompt.
                    _ => '/',
                };
                self.status.message = format!("{prompt_char}{needle}");
            }
            CtKey::Char(c) => {
                self.tabs[self.active_tab].editor_search.needle.push(c);
                self.refresh_editor_search_matches();
                self.jump_to_editor_search_match();
                let needle = self.tabs[self.active_tab].editor_search.needle.clone();
                let prompt_char = match self.tabs[self.active_tab].editor_search.direction {
                    SearchDirection::Forward => '/',
                    SearchDirection::Backward => '?',
                    // Future search directions: default to forward prompt.
                    _ => '/',
                };
                self.status.message = format!("{prompt_char}{needle}");
            }
            _ => {}
        }
    }

    /// Recompute all match positions for the current needle.
    fn refresh_editor_search_matches(&mut self) {
        let needle = self.tabs[self.active_tab].editor_search.needle.clone();
        if needle.is_empty() {
            self.tabs[self.active_tab].editor_search.matches.clear();
            self.tabs[self.active_tab].editor_search.current = None;
            return;
        }
        let text = self.tabs[self.active_tab].editor.entire_text();
        let matches = find_all(&text, &needle);
        self.tabs[self.active_tab].editor_search.matches = matches;
        self.sync_editor_search_current();
    }

    /// Jump the cursor to the best match given the current direction
    /// and saved cursor position.
    fn jump_to_editor_search_match(&mut self) {
        let tab = &self.tabs[self.active_tab];
        if tab.editor_search.matches.is_empty() {
            return;
        }
        let (cur_row, cur_col) = tab
            .editor_search
            .saved_cursor
            .unwrap_or_else(|| tab.editor.cursor());
        let direction = tab.editor_search.direction;
        let cursor_byte = row_col_to_offset(&tab.editor, cur_row, cur_col);

        let idx = match direction {
            SearchDirection::Forward => tab
                .editor_search
                .matches
                .iter()
                .position(|&(l, c)| {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    m_byte > cursor_byte
                })
                .or({
                    // Wrap around.
                    if !tab.editor_search.matches.is_empty() {
                        Some(0)
                    } else {
                        None
                    }
                }),
            SearchDirection::Backward => {
                // Find the last match before the cursor.
                let mut best: Option<usize> = None;
                for (i, &(l, c)) in tab.editor_search.matches.iter().enumerate() {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    if m_byte < cursor_byte {
                        best = Some(i);
                    } else {
                        break;
                    }
                }
                best.or_else(|| {
                    // Wrap around to the last match.
                    if !tab.editor_search.matches.is_empty() {
                        Some(tab.editor_search.matches.len() - 1)
                    } else {
                        None
                    }
                })
            }
            // Future search directions: treat as forward.
            _ => tab
                .editor_search
                .matches
                .iter()
                .position(|&(l, c)| {
                    let m_byte = row_col_to_offset(&tab.editor, l, c);
                    m_byte > cursor_byte
                })
                .or(if tab.editor_search.matches.is_empty() {
                    None
                } else {
                    Some(0)
                }),
        };

        if let Some(i) = idx {
            let (row, col) = self.tabs[self.active_tab].editor_search.matches[i];
            self.tabs[self.active_tab].editor.set_cursor(row, col);
            self.tabs[self.active_tab].editor_search.current = Some(i);
        }
    }

    /// Set `current` to the index of the match the cursor currently sits on.
    fn sync_editor_search_current(&mut self) {
        let tab = &self.tabs[self.active_tab];
        let (cur_row, cur_col) = tab.editor.cursor();
        let needle_len = tab.editor_search.needle.len();
        if needle_len == 0 {
            self.tabs[self.active_tab].editor_search.current = None;
            return;
        }
        let current = tab
            .editor_search
            .matches
            .iter()
            .position(|&(l, c)| l == cur_row && c <= cur_col && cur_col < c + needle_len)
            .or_else(|| {
                tab.editor_search
                    .matches
                    .iter()
                    .position(|&(l, c)| l == cur_row && c == cur_col)
            });
        self.tabs[self.active_tab].editor_search.current = current;
    }

    /// Repeat the editor search in the original or reverse direction.
    fn repeat_editor_search(&mut self, reverse: bool) {
        let tab = &self.tabs[self.active_tab];
        if tab.editor_search.needle.is_empty() {
            self.status.message = "no previous search".into();
            return;
        }
        if tab.editor_search.matches.is_empty() {
            self.status.message = format!("/{} · no matches", tab.editor_search.needle);
            return;
        }
        let direction = tab.editor_search.direction;
        let go_forward = match (direction, reverse) {
            (SearchDirection::Forward, false) => true,
            (SearchDirection::Forward, true) => false,
            (SearchDirection::Backward, false) => false,
            (SearchDirection::Backward, true) => true,
            // Future directions default to forward.
            (_, false) => true,
            (_, true) => false,
        };

        let count = tab.editor_search.matches.len();
        let cur = tab.editor_search.current.unwrap_or(0);
        let next = if go_forward {
            (cur + 1) % count
        } else {
            (cur + count - 1) % count
        };

        let (row, col) = self.tabs[self.active_tab].editor_search.matches[next];
        self.tabs[self.active_tab].editor.set_cursor(row, col);
        self.tabs[self.active_tab].editor_search.current = Some(next);
        let needle = self.tabs[self.active_tab].editor_search.needle.clone();
        self.status.message = format!("/{needle} · {}/{count}", next + 1);
    }

    // ----- substitute -----

    /// Execute a substitute command (`:s/old/new/[g][c]` or `:%s/old/new/[g][c]`).
    pub(super) fn execute_substitute(
        &mut self,
        range: crate::commands::SubstituteRange,
        pattern: &str,
        replacement: &str,
        global: bool,
        confirm: bool,
    ) {
        if confirm {
            // TODO(v1.1): implement interactive confirm mode with y/n/a/q.
            // For v1, execute all replacements and report via status message.
            self.status.message = "confirm flag not yet supported; replacing all matches".into();
        }

        let total_replacements = match range {
            crate::commands::SubstituteRange::CurrentLine => {
                let row = self.tabs[self.active_tab].editor.cursor_row();
                let line = self.tabs[self.active_tab].editor.get_line(row).to_owned();
                let (new_line, count) = if global {
                    replace_all(&line, pattern, replacement)
                } else {
                    replace_first(&line, pattern, replacement)
                };
                if count > 0 {
                    self.tabs[self.active_tab]
                        .editor
                        .replace_line(row, &new_line);
                }
                count
            }
            crate::commands::SubstituteRange::WholeBuffer => {
                let line_count = self.tabs[self.active_tab].editor.line_count();
                let mut total = 0usize;
                for row in 0..line_count {
                    let line = self.tabs[self.active_tab].editor.get_line(row).to_owned();
                    let (new_line, count) = if global {
                        replace_all(&line, pattern, replacement)
                    } else {
                        replace_first(&line, pattern, replacement)
                    };
                    if count > 0 {
                        self.tabs[self.active_tab]
                            .editor
                            .replace_line(row, &new_line);
                    }
                    total += count;
                }
                total
            }
        };

        if total_replacements == 0 {
            self.status.message = format!("{pattern} not found");
        } else {
            self.status.message = format!("{total_replacements} replacement(s) made");
        }
    }

    // ----- completion -----

    fn trigger_completion(&mut self) {
        let prefix = self.tabs[self.active_tab].editor.current_word_prefix();
        if prefix.is_empty() {
            // Empty prefix: behave like a plain insert (4 spaces).
            self.tabs[self.active_tab].editor.insert_str("    ");
            return;
        }
        let schemas = self
            .session
            .as_ref()
            .map(|s| s.schemas.as_slice())
            .unwrap_or(&[]);
        let known_schemas: Vec<String> = schemas.iter().map(|(s, _)| s.name.clone()).collect();
        let buffer_text = self.tabs[self.active_tab].editor.entire_text();
        let offset = self.tabs[self.active_tab].editor.cursor_byte_offset();
        let context = detect_context_with_schemas(&buffer_text, offset, &known_schemas);
        let columns = self.column_cache();
        let items = gather_completions(&prefix, schemas, &context, &columns, 50);
        if items.is_empty() {
            self.status.message = format!("no completions for '{prefix}'");
            return;
        }
        if items.len() == 1 {
            // Exactly one match: insert it without showing the popup.
            let only = items[0].text.clone();
            self.tabs[self.active_tab]
                .editor
                .replace_current_word_with(&only);
            self.status.message = format!("completed: {only}");
            return;
        }
        self.tabs[self.active_tab].completion = Some(CompletionState {
            items,
            selected: 0,
            prefix,
        });
        self.status.message = "completion: ↑↓ cycles · Tab/Enter accepts · Esc cancels".into();
    }

    /// Returns `true` when the key was consumed by the completion popup.
    ///
    /// Bindings inside the popup follow the IDE convention used by
    /// IntelliJ / DataGrip / VS Code so the muscle memory transfers:
    /// - Tab / Enter: accept the selected completion
    /// - ↑ / ↓: move the highlight
    /// - Shift-Tab: previous highlight (kept for keyboards without
    ///   arrow access in vim-aware terminal multiplexers)
    /// - Esc: dismiss the popup; the editor stays in insert mode and
    ///   the originally typed prefix is preserved
    fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(state) = self.tabs[self.active_tab].completion.as_mut() else {
            return false;
        };
        match key.code {
            CtKey::Esc => {
                self.tabs[self.active_tab].completion = None;
                self.status.message = "completion cancelled".into();
                true
            }
            CtKey::Enter | CtKey::Tab => {
                let choice = state.items[state.selected].text.clone();
                self.tabs[self.active_tab]
                    .editor
                    .replace_current_word_with(&choice);
                self.tabs[self.active_tab].completion = None;
                self.status.message = format!("completed: {choice}");
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
                self.tabs[self.active_tab].completion = None;
                false
            }
        }
    }

    pub(super) fn handle_sidebar_key(&mut self, key: KeyEvent) {
        // L24: paging step. The actual viewport size depends on the
        // terminal height; using a fixed step keeps the binding
        // predictable regardless of layout. Wheel events use a smaller
        // step (3) to feel closer to the OS scroll cadence.
        const PAGE_STEP: usize = 10;
        match key.code {
            CtKey::Char('j') | CtKey::Down if !self.sidebar_items.is_empty() => {
                self.sidebar_index = (self.sidebar_index + 1) % self.sidebar_items.len();
            }
            CtKey::Char('k') | CtKey::Up if !self.sidebar_items.is_empty() => {
                let len = self.sidebar_items.len();
                self.sidebar_index = (self.sidebar_index + len - 1) % len;
            }
            CtKey::PageDown | CtKey::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.sidebar_items.is_empty() =>
            {
                let len = self.sidebar_items.len();
                self.sidebar_index = (self.sidebar_index + PAGE_STEP).min(len - 1);
            }
            CtKey::PageUp | CtKey::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.sidebar_items.is_empty() =>
            {
                self.sidebar_index = self.sidebar_index.saturating_sub(PAGE_STEP);
            }
            CtKey::Home if !self.sidebar_items.is_empty() => {
                self.sidebar_index = 0;
            }
            CtKey::End if !self.sidebar_items.is_empty() => {
                self.sidebar_index = self.sidebar_items.len() - 1;
            }
            CtKey::Enter => self.activate_sidebar_selection(),
            CtKey::Char('o') => self.preview_sidebar_selection(),
            CtKey::Char('d') => self.ddl_sidebar_selection(),
            _ => {}
        }
    }

    /// L24: scroll the sidebar viewport by `delta` rows without moving
    /// the selection. Mouse-wheel handlers call this so the user can
    /// inspect off-screen rows before committing to a click.
    pub(super) fn scroll_sidebar(&mut self, delta: isize) {
        if self.sidebar_items.is_empty() {
            return;
        }
        let max = self.sidebar_items.len().saturating_sub(1);
        let new = (self.sidebar_scroll as isize + delta).clamp(0, max as isize) as usize;
        self.sidebar_scroll = new;
    }

    fn preview_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to preview".into();
            return;
        };
        self.run_preview(&schema, &name, 0);
    }

    /// Pressing `d` with a sidebar table focused fetches the DDL and
    /// injects it into the editor at the cursor. No auto-run — the
    /// user inspects and decides.
    fn ddl_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        let SidebarItem::Table { schema, name, .. } = item else {
            self.status.message = "select a table to fetch DDL".into();
            return;
        };
        self.inject_ddl(&schema, &name);
    }

    fn inject_ddl(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.fetch_ddl(&schema_owned, &name_owned).await
            })
        });
        match result {
            Ok(ddl) => {
                self.tabs[self.active_tab].editor.insert_str(&ddl);
                self.status.message = format!("injected DDL for {schema}.{name}");
                self.focus = Pane::Editor;
            }
            Err(e) => {
                self.status.message = format!("DDL fetch failed: {e}");
            }
        }
    }

    /// Dispatch a `SELECT * FROM schema.table LIMIT n OFFSET k` and attach
    /// the table's schema as the result's row source so cell edits and
    /// pagination work.
    fn run_preview(&mut self, schema: &str, table: &str, offset: usize) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let dialect = session.dialect();
        let limit = self.tabs[self.active_tab].page_size;
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = table.to_owned();
        let described = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        let source = match described {
            Ok(ts) => {
                let columns = ts.columns.clone();
                // Cache column names for completion.
                if let Some(session) = self.session.as_mut() {
                    session.column_cache.insert(
                        table.to_ascii_lowercase(),
                        (
                            schema.to_owned(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                Some(RowSource {
                    schema: schema.to_owned(),
                    table: table.to_owned(),
                    columns,
                    offset,
                    limit,
                })
            }
            Err(error) => {
                debug!(target: "narwhal::app", error = %error, "describe_table for preview failed; rows will be read-only");
                None
            }
        };
        let sql = crate::ddl::preview_query_paged(schema, table, limit, offset, dialect);
        self.tabs[self.active_tab].pending_source = source;
        self.dispatch_batch(vec![sql], RunMode::Execute);
        self.focus = Pane::Results;
    }

    pub(super) fn next_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        let limit = self.tabs[self.active_tab].page_size;
        self.run_preview(&schema, &table, offset + limit);
    }

    pub(super) fn prev_page(&mut self) {
        let Some((schema, table, offset)) = self.current_preview_target() else {
            self.status.message = "no preview to paginate; select a table first".into();
            return;
        };
        if offset == 0 {
            self.status.message = "already on the first page".into();
            return;
        }
        let limit = self.tabs[self.active_tab].page_size;
        let new_offset = offset.saturating_sub(limit);
        self.run_preview(&schema, &table, new_offset);
    }

    pub(super) fn set_page_size(&mut self, size: usize) {
        self.tabs[self.active_tab].page_size = size;
        self.status.message = format!("page size set to {size}");
    }

    fn current_preview_target(&self) -> Option<(String, String, usize)> {
        match self.tabs[self.active_tab].results.active_state() {
            ResultState::Rows {
                source: Some(s), ..
            } => Some((s.schema.clone(), s.table.clone(), s.offset)),
            _ => None,
        }
    }

    fn activate_sidebar_selection(&mut self) {
        let Some(item) = self.sidebar_items.get(self.sidebar_index).cloned() else {
            return;
        };
        match item {
            SidebarItem::Connection { name, .. } => self.open_named(&name),
            SidebarItem::Schema { .. } => {}
            SidebarItem::Table { schema, name, .. } => {
                self.describe_table_into_result(&schema, &name);
            }
        }
    }

    fn describe_table_into_result(&mut self, schema: &str, name: &str) {
        let Some(session) = self.session.as_ref() else {
            self.status.message = "no active connection".into();
            return;
        };
        let pool = session.pool.clone();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut conn = pool
                    .acquire()
                    .await
                    .map_err(|e| narwhal_core::Error::Connection(e.to_string()))?;
                conn.describe_table(&schema_owned, &name_owned).await
            })
        });
        match result {
            Ok(ts) => {
                let col_count = ts.columns.len();
                let idx_count = ts.indexes.len();
                let fk_count = ts.foreign_keys.len();
                let table_schema = ts.table.schema.clone();
                let table_name = ts.table.name.clone();
                let columns = ts.columns.clone();
                // Cache column names for completion.
                if let Some(session) = self.session.as_mut() {
                    session.column_cache.insert(
                        table_name.to_ascii_lowercase(),
                        (
                            table_schema.clone(),
                            columns
                                .iter()
                                .map(|c| ColumnHeader {
                                    name: c.name.clone(),
                                    data_type: c.data_type.clone(),
                                })
                                .collect(),
                        ),
                    );
                }
                self.tabs[self.active_tab].results.active_mut().reset();
                self.status.message = format!(
                    "{}.{}: {} cols·{} idx·{} fk",
                    table_schema, table_name, col_count, idx_count, fk_count
                );
                *self.tabs[self.active_tab].results.active_state_mut() =
                    ResultState::TableDetail { schema: ts };
            }
            Err(error) => {
                self.status.message = format!("describe failed: {error}");
            }
        }
    }

    // handle_results_key, selected_original_row, yank_cell, yank_row,
    // start_cell_edit, handle_cell_edit_key, sync_edit_view, commit_cell_edit,
    // set_edit_error, start_search, toggle_sort, open_filter_prompt,
    // refresh_search_matches, advance_search, jump_to_current_match,
    // open_cell_popup, open_row_detail, handle_row_detail_key moved to
    // `core::results_actions` (L21).

    pub(super) fn apply_action(&mut self, action: Action) {
        match action {
            Action::Move { motion, count } => {
                self.tabs[self.active_tab]
                    .editor
                    .apply_motion(motion, count);
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

    // ----- prompt tab-completion -----

    /// Complete the last token in the `:`-prompt buffer against the
    /// universe appropriate for the current command head.
    ///
    /// - `:open <pref>`, `:remove <pref>`, `:rm <pref>`, `:forget <pref>`
    ///   → connection names from `ConnectionsFile`
    /// - `:help <pref>` → built-in command names ∪ plugin command names
    /// - `:export <pref>` → `csv` | `json`
    /// - bare `:` (empty buffer) → no completion (too noisy)
    /// - any other head → no-op
    fn complete_prompt(&mut self) {
        let buf = self.vim.command_buffer().to_owned();
        let parts: Vec<&str> = buf.split_whitespace().collect();
        let head = parts.first().copied().unwrap_or("");

        // Identify which universe to complete from.
        let universe: Vec<String> = match head {
            "open" | "o" | "remove" | "rm" | "forget" => self
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
                self.snippet_store.list().unwrap_or_default()
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
                self.status.message = format!("no completions for {prefix:?}");
            }
            [only] => {
                self.vim.replace_command_token(only);
                self.status.message = format!(":{}", self.vim.command_buffer());
            }
            many => {
                let lcp = longest_common_prefix(many);
                if lcp.len() > prefix.len() {
                    self.vim.replace_command_token(&lcp);
                }
                self.status.message = many.join(" ");
            }
        }
    }
}
