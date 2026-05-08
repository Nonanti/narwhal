//! Tiny text helpers extracted from `core.rs` (L21).
//!
//! These functions are pure, allocation-light, and do not touch [`AppCore`]
//! state. They live in their own module so the main module stays focused on
//! event handling and state transitions.

use narwhal_domain::editor::EditorBuffer;

/// Split a one-line command argument into `(head, tail)` where `head` is the
/// first whitespace-delimited token and `tail` is the rest with leading
/// whitespace trimmed.
pub(super) fn split_head_arg(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
        None => (trimmed, ""),
    }
}

/// Truncate a string to at most `max` bytes, replacing the tail with `…`
/// while respecting char boundaries.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut end = max.saturating_sub(1);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Compute the longest common prefix across a non-empty slice of strings,
/// character by character.
pub(super) fn longest_common_prefix(strings: &[&str]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = strings[0];
    let mut end = 0;
    for (i, ch) in first.char_indices() {
        if strings[1..].iter().all(|s| s.chars().nth(i) == Some(ch)) {
            end = i + ch.len_utf8();
        } else {
            break;
        }
    }
    first[..end].to_owned()
}

/// Find all occurrences of `needle` in `buffer`, returning
/// `(line_idx, byte_col)` pairs. Literal substring, no regex.
pub(super) fn find_all(buffer: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in buffer.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line[start..].find(needle) {
            out.push((line_idx, start + pos));
            // L32: the empty-needle case returns early above, so
            // `needle.len()` is always >= 1; the old `.max(1)` was dead.
            start += pos + needle.len();
        }
    }
    out
}

/// Convert a (row, col) position in the editor buffer to a byte offset.
pub(super) fn row_col_to_offset(buffer: &EditorBuffer, row: usize, col: usize) -> usize {
    let mut offset = 0usize;
    for (i, line) in buffer.lines().iter().enumerate() {
        if i == row {
            return offset + col.min(line.len());
        }
        offset += line.len() + 1; // +1 for the synthetic newline
    }
    offset
}

/// Replace the first occurrence of `pattern` with `replacement` in `text`.
/// Returns the new string and the number of replacements (0 or 1).
pub(super) fn replace_first(text: &str, pattern: &str, replacement: &str) -> (String, usize) {
    if let Some(pos) = text.find(pattern) {
        let mut result = String::with_capacity(text.len() + replacement.len());
        result.push_str(&text[..pos]);
        result.push_str(replacement);
        result.push_str(&text[pos + pattern.len()..]);
        (result, 1)
    } else {
        (text.to_owned(), 0)
    }
}

/// Replace every occurrence of `pattern` with `replacement` in `text`.
/// Returns the new string and the count of replacements.
pub(super) fn replace_all(text: &str, pattern: &str, replacement: &str) -> (String, usize) {
    if pattern.is_empty() {
        return (text.to_owned(), 0);
    }
    let mut result = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut start = 0;
    while let Some(pos) = text[start..].find(pattern) {
        result.push_str(&text[start..start + pos]);
        result.push_str(replacement);
        start += pos + pattern.len();
        count += 1;
    }
    result.push_str(&text[start..]);
    (result, count)
}
