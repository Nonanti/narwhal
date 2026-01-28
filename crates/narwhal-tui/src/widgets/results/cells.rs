//! Cell-level helpers: width computation, grid sanitisation,
//! safe display for display-control characters.

use narwhal_core::{ColumnHeader, Row};
use unicode_width::UnicodeWidthStr;

use crate::constants::{
    RESULT_MAX_COLUMN_WIDTH as MAX_COLUMN_WIDTH, RESULT_MIN_COLUMN_WIDTH as MIN_COLUMN_WIDTH,
};

pub(super) fn compute_column_widths(columns: &[ColumnHeader], rows: &[Row]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let header_len = format!("{} ({})", c.name, c.data_type).width();
            let body_len = rows
                .iter()
                .map(|r| {
                    r.0.get(i)
                        .map_or(0, |v| render_for_grid(&v.render()).width())
                })
                .max()
                .unwrap_or(0);
            header_len
                .max(body_len)
                .clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
        })
        .collect()
}

/// Single-line projection used in the result grid. Cell popup still shows
/// the raw value through a `Paragraph` widget so the user can read the
/// real text on demand — this just keeps grid rows one row tall.
///
/// Also sanitises dangerous Unicode glyphs (BIDI overrides, zero-width
/// characters, control chars) that could be used for visual spoofing
/// (Trojan Source attacks). Such characters are replaced with `·`.
pub(super) fn render_for_grid(s: &str) -> String {
    let mut needs_sanitize = false;
    let mut needs_newline_replace = false;
    for ch in s.chars() {
        if is_dangerous_glyph(ch) {
            needs_sanitize = true;
            break;
        }
        if matches!(ch, '\n' | '\r' | '\t') {
            needs_newline_replace = true;
        }
    }
    if !needs_sanitize && !needs_newline_replace {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if is_dangerous_glyph(c) {
            out.push('·');
        } else {
            match c {
                '\r' => {
                    // Collapse CRLF into one glyph.
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    out.push('⏎');
                }
                '\n' => out.push('⏎'),
                '\t' => out.push('→'),
                other => out.push(other),
            }
        }
    }
    out
}

/// Returns true for Unicode characters that are dangerous to render
/// in a terminal grid: BIDI override controls, zero-width / directional
/// marks, and C0/C1 control characters (except \t, \n, \r which are
/// handled separately by `render_for_grid`).
pub(super) const fn is_dangerous_glyph(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'  // BIDI override
        | '\u{2066}'..='\u{2069}' // BIDI isolate
        | '\u{200B}'..='\u{200F}' // zero-width, LRM/RLM
        | '\u{0000}'..='\u{0008}' // C0 controls (except TAB at 0x09)
        | '\u{000B}'..='\u{000C}' // VT, FF
        | '\u{000E}'..='\u{001F}' // SO..US, C1 range start
        | '\u{007F}'               // DEL
    )
}

/// Sanitise a string for display in any TUI context (cell popup,
/// row detail, history, sidebar, status). Replaces BIDI override
/// characters, zero-width / directional marks, and C0/C1 control
/// characters with `·`. Unlike `render_for_grid`, this does **not**
/// replace newlines / tabs — callers that need single-line projection
/// should use `render_for_grid` instead.
pub fn sanitize_for_display(s: &str) -> std::borrow::Cow<'_, str> {
    let needs = s.chars().any(is_dangerous_glyph);
    if !needs {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if is_dangerous_glyph(ch) {
            out.push('·');
        } else {
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}
