//! Regression tests for editor cursor / `set_cursor` unicode handling (bug C4).
//!
//! Two failure modes the pre-fix code suffers from:
//!
//! 1. Visual: `cursor_x` was computed from `cursor_col` (a *byte* offset),
//!    so multi-byte characters (Turkish, CJK, emoji) shifted the cursor
//!    sprite away from the actual glyph and broke completion-popup
//!    placement.
//! 2. Panic: `set_cursor` clamped to `line.len()` but did not snap to a
//!    char boundary, so later edits (`insert_char`, `delete_char`,
//!    `insert_str("\n")`) panicked inside `String::insert` / `split_off`.

use narwhal_tui::widgets::EditorBuffer;
use narwhal_tui::widgets::editor::editor_cursor_anchor;
use ratatui::layout::Rect;

const GUTTER_WIDTH: u16 = 6;

#[test]
fn cursor_x_respects_display_width_for_multibyte() {
    let mut buf = EditorBuffer::new();
    buf.insert_str("şahin");
    // After inserting `şahin` (5 grapheme/display columns, 6 bytes since
    // `ş` is 2 bytes), the cursor should appear 5 cells past the gutter,
    // not 6.
    let area = Rect::new(0, 0, 80, 24);
    let (x, _y) = editor_cursor_anchor(area, &buf);
    let inner_x = area.x + 1; // matches render_editor's inner rect
    assert_eq!(
        x - inner_x,
        GUTTER_WIDTH + 5,
        "cursor x should advance by display width (5), not byte length (6)"
    );
}

#[test]
fn set_cursor_snaps_back_to_char_boundary() {
    let mut buf = EditorBuffer::new();
    buf.insert_str("aü"); // bytes: a(0) ü(1,2), len = 3
    // Try to place the cursor in the middle of `ü` (byte 2).
    buf.set_cursor(0, 2);
    let (_, col) = buf.cursor();
    assert!(
        col == 1 || col == 3,
        "cursor should land on a char boundary (1 or 3), got {col}"
    );
}

#[test]
fn insert_char_after_set_cursor_does_not_panic() {
    let mut buf = EditorBuffer::new();
    buf.insert_str("aü"); // 3 bytes
    // Land on a multibyte midpoint and then mutate.
    buf.set_cursor(0, 2);
    buf.insert_char('x');
    // No panic = pass. Content sanity:
    let text = buf.entire_text();
    assert!(text.contains('x'));
    assert!(text.contains('ü'));
}

#[test]
fn insert_newline_after_set_cursor_does_not_panic() {
    let mut buf = EditorBuffer::new();
    buf.insert_str("aü");
    buf.set_cursor(0, 2);
    buf.insert_str("\n");
    assert_eq!(buf.line_count(), 2);
}
