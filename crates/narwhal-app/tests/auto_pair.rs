//! Integration tests for auto-pair brackets and quotes in insert mode.

use narwhal_tui::EditorBuffer;

/// Helper: insert a character via `insert_char` on a fresh buffer and return it.
fn buf_with_chars(chars: &[char]) -> EditorBuffer {
    let mut buf = EditorBuffer::new();
    for &c in chars {
        buf.insert_char(c);
    }
    buf
}

#[test]
fn open_paren_inserts_matched_pair() {
    let buf = buf_with_chars(&['(']);
    assert_eq!(buf.lines(), &["()"]);
    assert_eq!(buf.cursor(), (0, 1)); // cursor between ( and )
}

#[test]
fn close_paren_skips_existing_close() {
    let mut buf = EditorBuffer::new();
    buf.insert_char('('); // yields () with cursor at col 1
    buf.insert_char(')'); // should skip over, cursor at col 2
    assert_eq!(buf.lines(), &["()"]);
    assert_eq!(buf.cursor(), (0, 2));
}

#[test]
fn quotes_pair() {
    let buf = buf_with_chars(&['\'']);
    assert_eq!(buf.lines(), &["''"]);
    assert_eq!(buf.cursor(), (0, 1));
}

#[test]
fn backspace_inside_empty_pair_deletes_both() {
    let mut buf = EditorBuffer::new();
    buf.insert_char('('); // () cursor at 1
    buf.delete_char(); // should delete both
    assert_eq!(buf.lines(), &[""]);
    assert_eq!(buf.cursor(), (0, 0));
}

#[test]
fn no_pair_inside_string_literal() {
    let mut buf = EditorBuffer::new();
    buf.set_auto_pair_enabled(true);
    // Build: 'where x =' with cursor at end — we need to get inside a
    // single-quoted string. Insert opening quote (auto-paired to ''),
    // then move left back inside and type content.
    buf.insert_char('\''); // '' cursor at 1 (inside)
                           // Type content inside the string
    for c in "where x =".chars() {
        buf.insert_char(c);
    }
    // Now cursor should be just before the closing ' of the string.
    // Inserting ( should NOT auto-pair because we're inside a string.
    buf.insert_char('(');
    // Should be single (, not ()
    assert_eq!(buf.lines(), &["'where x =('"]);
}

#[test]
fn no_pair_when_next_char_is_opener() {
    let mut buf = EditorBuffer::new();
    // Start with () via auto-pair, then move cursor to before (
    buf.insert_char('('); // () with cursor at 1
    buf.insert_char(')'); // skip over, cursor at 2 (after ))
                          // Now go to start and type ( before the existing pair
    buf.apply_motion(narwhal_vim::Motion::LineStart, 1);
    buf.insert_char('(');
    // Should NOT over-pair: buffer should be ((), not (())(
    assert_eq!(buf.lines(), &["(()"]);
}

#[test]
fn non_pair_characters_unaffected() {
    let buf = buf_with_chars(&['s', 'e', 'l']);
    assert_eq!(buf.lines(), &["sel"]);
    assert_eq!(buf.cursor(), (0, 3));
}

#[test]
fn nested_pairs() {
    let mut buf = EditorBuffer::new();
    buf.insert_char('('); // () cursor at 1
                          // Next char is ), which is not an opener, so ( should auto-pair
    buf.insert_char('('); // (()) cursor at 2
    buf.insert_char(')'); // skip inner ), cursor at 3
    buf.insert_char(')'); // skip outer ), cursor at 4
    assert_eq!(buf.lines(), &["(())"]);
    assert_eq!(buf.cursor(), (0, 4));
}
