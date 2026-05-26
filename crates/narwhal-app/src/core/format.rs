//! `:format` / `:format-all` implementations.
//!
//! Both commands rebuild the editor buffer from the splitter output
//! so the formatter doesn't have to know about cursor positions or
//! line/column maths. The buffer's `clear` + `insert_str` cycle
//! handles all the bookkeeping (cursor row/col, scroll offset).
//!
//! The dialect is taken from the active session when present and
//! falls back to [`Dialect::Generic`] otherwise — formatting still
//! works without a connection so users can clean up snippets before
//! ever opening one.

use narwhal_sql::{format_for_driver, split_with, Dialect};

use super::AppCore;

impl AppCore {
    pub(super) async fn format_current_statement(&mut self) {
        let dialect = self.active_dialect().await;
        let driver_name = self.active_driver_name().await;
        let text = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        let cursor_offset = self.ui.tabs[self.ui.active_tab].editor.cursor_byte_offset();
        let stmts = split_with(&text, dialect);
        if stmts.is_empty() {
            self.ui.status.message = "format: nothing to format".into();
            return;
        }
        let target_idx = stmts
            .iter()
            .position(|s| cursor_offset >= s.start && cursor_offset <= s.end)
            .unwrap_or(stmts.len() - 1);

        let mut rewritten: Vec<String> = stmts
            .iter()
            .map(|s| s.text.trim().trim_end_matches(';').trim().to_owned())
            .collect();
        rewritten[target_idx] = format_for_driver(&rewritten[target_idx], &driver_name);

        let new_text = join_statements(&rewritten);
        let cursor_target = cursor_target_for(&rewritten, target_idx);
        replace_editor_contents(self, &new_text, cursor_target);
        self.ui.status.message = format!("formatted statement {}/{}", target_idx + 1, stmts.len());
    }

    pub(super) async fn format_all_statements(&mut self) {
        let dialect = self.active_dialect().await;
        let driver_name = self.active_driver_name().await;
        let text = self.ui.tabs[self.ui.active_tab].editor.entire_text();
        let stmts = split_with(&text, dialect);
        if stmts.is_empty() {
            self.ui.status.message = "format-all: nothing to format".into();
            return;
        }
        let formatted: Vec<String> = stmts
            .iter()
            .map(|s| {
                let body = s.text.trim().trim_end_matches(';').trim();
                format_for_driver(body, &driver_name)
            })
            .collect();
        let new_text = join_statements(&formatted);
        replace_editor_contents(self, &new_text, 0);
        self.ui.status.message = format!("formatted {} statement(s)", stmts.len());
    }

    async fn active_dialect(&self) -> Dialect {
        self.session
            .active
            .as_ref()
            .map_or(Dialect::Generic, super::super::session::Session::dialect)
    }

    async fn active_driver_name(&self) -> String {
        self.session
            .active
            .as_ref()
            .map_or_else(|| "generic".to_owned(), |s| s.driver.name().to_owned())
    }
}

/// Glue statements together with a blank line separator. Every
/// statement keeps its trailing semicolon so the editor buffer is
/// runnable as-is.
fn join_statements(stmts: &[String]) -> String {
    let mut out = String::new();
    for (i, s) in stmts.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(s.trim());
        out.push(';');
    }
    out
}

/// Byte offset to drop the cursor at after a successful format: the
/// start of the formatted statement so the user can keep editing
/// inside it.
fn cursor_target_for(stmts: &[String], target_idx: usize) -> usize {
    let mut offset = 0;
    for (i, s) in stmts.iter().enumerate().take(target_idx) {
        offset += s.trim().len() + 1; // body + ';'
        if i + 1 < stmts.len() {
            offset += 2; // "\n\n" separator
        }
    }
    offset
}

fn replace_editor_contents(core: &mut AppCore, text: &str, cursor_byte_offset: usize) {
    let editor = &mut core.ui.tabs[core.ui.active_tab].editor;
    editor.clear();
    editor.insert_str(text);
    // Walk the buffer to translate byte offset into (row, col).
    let mut row = 0usize;
    let mut col = 0usize;
    let mut walked = 0usize;
    for ch in text.chars() {
        if walked >= cursor_byte_offset {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += ch.len_utf8();
        }
        walked += ch.len_utf8();
    }
    editor.set_cursor(row, col);
}
