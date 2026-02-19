//! Integration tests for editor search (/ ? n N) and substitute (:s/:%s).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_vim::SearchDirection;

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_core() -> AppCore {
    AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forward_search_finds_first_match() {
    let mut core = make_core();
    core.insert_into_editor("SELECT users FROM users").await;
    // Place cursor at the beginning.
    core.execute_command("clear").await;
    core.insert_into_editor("SELECT users FROM users").await;

    // Press / to open forward search prompt.
    core.handle_key(key(KeyCode::Char('/'))).await;
    assert!(core.tabs()[core.active_tab()].editor_search().prompt_open);
    assert_eq!(
        core.tabs()[core.active_tab()].editor_search().direction,
        SearchDirection::Forward
    );

    // Type "users" character by character.
    for c in "users".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    // Press Enter to accept.
    core.handle_key(key(KeyCode::Enter)).await;

    // The cursor should be on the first "users" (line 0, col 7).
    let (row, col) = core.editor().cursor();
    assert_eq!(row, 0);
    assert_eq!(col, 7, "cursor should be at start of first 'users' match");
    // Highlighting should be active.
    assert!(core.tabs()[core.active_tab()].editor_search().highlight);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backward_search_finds_first_match() {
    let mut core = make_core();
    core.insert_into_editor("SELECT users FROM users").await;

    // Move cursor to end of line first.
    core.handle_key(key(KeyCode::Char('$'))).await;

    // Press ? to open backward search prompt.
    core.handle_key(key(KeyCode::Char('?'))).await;
    assert!(core.tabs()[core.active_tab()].editor_search().prompt_open);
    assert_eq!(
        core.tabs()[core.active_tab()].editor_search().direction,
        SearchDirection::Backward
    );

    // Type "users" character by character.
    for c in "users".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;

    // Backward search from the end should find the second "users" first.
    let (row, col) = core.editor().cursor();
    assert_eq!(row, 0);
    assert_eq!(
        col, 18,
        "cursor should be at start of second 'users' match (backward)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_repeats_search_forward() {
    let mut core = make_core();
    core.insert_into_editor("foo bar foo baz foo").await;

    // Open forward search, type "foo", accept.
    core.handle_key(key(KeyCode::Char('/'))).await;
    for c in "foo".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;

    // First match at col 0.
    let (_, col) = core.editor().cursor();
    assert_eq!(col, 0);

    // Press n to go to next match.
    core.handle_key(key(KeyCode::Char('n'))).await;
    let (_, col) = core.editor().cursor();
    assert_eq!(col, 8, "n should jump to the second 'foo' at col 8");

    // Press n again to go to third match.
    core.handle_key(key(KeyCode::Char('n'))).await;
    let (_, col) = core.editor().cursor();
    assert_eq!(col, 16, "n should jump to the third 'foo' at col 16");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capital_n_repeats_in_opposite_direction() {
    let mut core = make_core();
    core.insert_into_editor("foo bar foo baz foo").await;

    // Open forward search, type "foo", accept.
    core.handle_key(key(KeyCode::Char('/'))).await;
    for c in "foo".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;

    // Advance to second match.
    core.handle_key(key(KeyCode::Char('n'))).await;
    let (_, col) = core.editor().cursor();
    assert_eq!(col, 8);

    // Press N to go back to first match.
    core.handle_key(key(KeyCode::Char('N'))).await;
    let (_, col) = core.editor().cursor();
    assert_eq!(col, 0, "N should go back to the first 'foo'");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_during_prompt_restores_cursor() {
    let mut core = make_core();
    core.insert_into_editor("hello world").await;

    // Move cursor right a few times to have a known position.
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('l'))).await;
    let (orig_row, orig_col) = core.editor().cursor();

    // Open search prompt.
    core.handle_key(key(KeyCode::Char('/'))).await;
    // Type something.
    for c in "world".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    // Cancel with Esc.
    core.handle_key(key(KeyCode::Esc)).await;

    let (row, col) = core.editor().cursor();
    assert_eq!(
        (row, col),
        (orig_row, orig_col),
        "Esc should restore cursor to pre-search position"
    );
    // Search should be cleared.
    assert!(!core.tabs()[core.active_tab()].editor_search().prompt_open);
    assert!(!core.tabs()[core.active_tab()].editor_search().highlight);
    assert!(core.tabs()[core.active_tab()]
        .editor_search()
        .needle
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_during_prompt_keeps_match_highlighted() {
    let mut core = make_core();
    core.insert_into_editor("find this word").await;

    core.handle_key(key(KeyCode::Char('/'))).await;
    for c in "this".chars() {
        core.handle_key(key(KeyCode::Char(c))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;

    // Prompt should be closed but highlights should remain.
    assert!(!core.tabs()[core.active_tab()].editor_search().prompt_open);
    assert!(core.tabs()[core.active_tab()].editor_search().highlight);
    assert_eq!(
        core.tabs()[core.active_tab()].editor_search().needle,
        "this"
    );
    assert!(!core.tabs()[core.active_tab()]
        .editor_search()
        .matches
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substitute_current_line_no_g() {
    let mut core = make_core();
    core.insert_into_editor("foo foo foo").await;

    // :s/foo/bar/ replaces only the first occurrence on the current line.
    core.execute_command("s/foo/bar/").await;

    let text = core.editor().entire_text();
    assert_eq!(
        text, "bar foo foo",
        "without g flag, only first occurrence should be replaced"
    );
    assert!(core.status_message().contains("1 replacement"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substitute_current_line_g() {
    let mut core = make_core();
    core.insert_into_editor("foo foo foo").await;

    // :s/foo/bar/g replaces every occurrence on the current line.
    core.execute_command("s/foo/bar/g").await;

    let text = core.editor().entire_text();
    assert_eq!(
        text, "bar bar bar",
        "with g flag, all occurrences on current line should be replaced"
    );
    assert!(core.status_message().contains("3 replacement"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substitute_whole_buffer() {
    let mut core = make_core();
    core.insert_into_editor("foo line1\nfoo line2\nfoo line3")
        .await;

    // :%s/foo/bar/g replaces every occurrence in the whole buffer.
    core.execute_command("%s/foo/bar/g").await;

    let text = core.editor().entire_text();
    assert_eq!(text, "bar line1\nbar line2\nbar line3");
    assert!(core.status_message().contains("3 replacement"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substitute_no_match_status_message() {
    let mut core = make_core();
    core.insert_into_editor("hello world").await;

    core.execute_command("s/xyz/abc/").await;

    let text = core.editor().entire_text();
    assert_eq!(
        text, "hello world",
        "buffer should be unchanged when pattern not found"
    );
    assert!(
        core.status_message().contains("not found"),
        "status should report pattern not found"
    );
}
