//! SQL statement extraction over an editor buffer.
//!
//! The text buffer itself lives in `narwhal-tui` and is dialect-agnostic; the
//! statement-extraction logic here is the *only* place that bridges the editor
//! to `narwhal-sql::Splitter`. Keeping it in `narwhal-app` lets `narwhal-tui`
//! stay reusable for alternative backends (Helix, GPUI, …) without dragging
//! the SQL splitter into the UI crate (bug.md H18).

use narwhal_sql::{split_with, Dialect};
use narwhal_domain::editor::EditorBuffer;

/// Return every statement in the buffer, trimmed of surrounding whitespace
/// and of any trailing semicolon.
pub fn all_statements(buf: &EditorBuffer, dialect: Dialect) -> Vec<String> {
    let text = buf.entire_text();
    split_with(&text, dialect)
        .into_iter()
        .filter_map(|s| {
            let cleaned = s.text.trim().trim_end_matches(';').trim().to_owned();
            (!cleaned.is_empty()).then_some(cleaned)
        })
        .collect()
}

/// Extract the statement under the cursor.
///
/// Returns the full statement text including any trailing semicolon, or
/// `None` when the buffer contains no statements at all.
pub fn statement_at_cursor(buf: &EditorBuffer, dialect: Dialect) -> Option<String> {
    let text = buf.entire_text();
    let cursor_offset = buf.cursor_byte_offset();
    let statements = split_with(&text, dialect);
    if statements.is_empty() {
        return None;
    }
    for stmt in &statements {
        if cursor_offset >= stmt.start && cursor_offset <= stmt.end {
            return Some(stmt.text.to_owned());
        }
    }
    // Cursor is past the last statement end (trailing whitespace);
    // return the last statement encountered.
    statements.last().map(|s| s.text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_vim::Motion;

    #[test]
    fn statement_under_cursor_picks_the_right_one() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT 1; SELECT 2; SELECT 3");
        buf.apply_motion(Motion::LineStart, 1);
        let first = statement_at_cursor(&buf, Dialect::Generic).expect("statement");
        assert!(first.starts_with("SELECT 1"));

        // Walk into the second statement.
        for _ in 0..12 {
            buf.apply_motion(Motion::Right, 1);
        }
        let second = statement_at_cursor(&buf, Dialect::Generic).expect("statement");
        assert!(second.contains("SELECT 2"));
    }

    #[test]
    fn all_statements_splits_and_trims() {
        let mut buf = EditorBuffer::new();
        buf.insert_str("SELECT 1; SELECT 2;\n  ; SELECT 3");
        let stmts = all_statements(&buf, Dialect::Generic);
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2", "SELECT 3"]);
    }

    #[test]
    fn statement_at_cursor_empty_buffer_is_none() {
        let buf = EditorBuffer::new();
        assert!(statement_at_cursor(&buf, Dialect::Generic).is_none());
    }
}
