//! Editor in-pane search and `:s/old/new` substitution.

use crossterm::event::{KeyCode as CtKey, KeyEvent};
use narwhal_vim::SearchDirection;

use crate::core::text_utils::{
    find_all, replace_all, replace_first, row_col_to_offset,
};
use crate::core::AppCore;

impl AppCore {
    pub(crate) fn open_editor_search(&mut self, direction: SearchDirection) {
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
    pub(crate) fn handle_editor_search_key(&mut self, key: KeyEvent) {
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
                        .map_or(1, |i| i + 1);
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
    pub(crate) fn refresh_editor_search_matches(&mut self) {
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
    pub(crate) fn jump_to_editor_search_match(&mut self) {
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
                    if tab.editor_search.matches.is_empty() {
                        None
                    } else {
                        Some(0)
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
                    if tab.editor_search.matches.is_empty() {
                        None
                    } else {
                        Some(tab.editor_search.matches.len() - 1)
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
    pub(crate) fn sync_editor_search_current(&mut self) {
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
    pub(crate) fn repeat_editor_search(&mut self, reverse: bool) {
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


    /// Execute a substitute command (`:s/old/new/[g][c]` or `:%s/old/new/[g][c]`).
    pub(crate) fn execute_substitute(
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


}
