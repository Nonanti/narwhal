//! Integration tests for the basic (modeless) editor handler.
//!
//! Drives [`AppCore`] with `editor.mode = "basic"` and asserts that
//! the IDE-style chord set produces the same buffer state a user
//! would expect from any GUI editor: arrow keys move the cursor,
//! Shift+arrow extends a selection, Ctrl+C/V/X round-trip through
//! the in-memory clipboard, and Ctrl+Z / Ctrl+Y traverse the undo
//! history that the snapshot pipeline records on every mutation.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::clipboard::{Clipboard, InMemoryClipboard};
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, EditorMode, InMemoryStore, Settings};
use std::sync::Arc;

const fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

const fn plain(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

const fn ctrl(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

const fn shift(code: KeyCode) -> KeyEvent {
    key(code, KeyModifiers::SHIFT)
}

fn basic_core_with_clipboard(clipboard: Arc<dyn Clipboard>) -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_services(
        registry,
        ConnectionsFile::default(),
        None,
        Arc::new(InMemoryStore::new()),
        clipboard,
    );
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    core.apply_settings(settings);
    core
}

fn basic_core() -> AppCore {
    basic_core_with_clipboard(Arc::new(InMemoryClipboard::new()))
}

fn buffer_text(core: &AppCore) -> String {
    core.editor().entire_text()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_typing_inserts_characters() {
    let mut core = basic_core();
    for c in ['h', 'e', 'l', 'l', 'o'] {
        core.handle_key(plain(c)).await;
    }
    assert_eq!(buffer_text(&core), "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arrow_navigation_moves_without_inserting() {
    let mut core = basic_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
        .await;
    core.handle_key(plain('X')).await;
    assert_eq!(buffer_text(&core), "abXc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_inserts_newline() {
    let mut core = basic_core();
    core.handle_key(plain('a')).await;
    core.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .await;
    core.handle_key(plain('b')).await;
    assert_eq!(buffer_text(&core), "a\nb");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backspace_deletes_left_of_cursor() {
    let mut core = basic_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .await;
    assert_eq!(buffer_text(&core), "ab");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shift_arrow_extends_selection() {
    let mut core = basic_core();
    for c in ['a', 'b', 'c', 'd', 'e'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(shift(KeyCode::Left)).await;
    core.handle_key(shift(KeyCode::Left)).await;
    assert!(core.editor().has_selection());
    let selected = core.editor().selected_text();
    assert_eq!(selected, "de");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_a_selects_all() {
    let mut core = basic_core();
    for c in ['x', 'y', 'z'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    assert_eq!(core.editor().selected_text(), "xyz");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_copies_selection_to_clipboard() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = basic_core_with_clipboard(clipboard.clone());
    for c in ['c', 'o', 'p', 'y'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(ctrl('c')).await;
    assert_eq!(clipboard.read().as_deref(), Some("copy"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_x_cuts_selection() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = basic_core_with_clipboard(clipboard.clone());
    for c in ['c', 'u', 't', '!'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(ctrl('x')).await;
    assert_eq!(clipboard.read().as_deref(), Some("cut!"));
    assert_eq!(buffer_text(&core), "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_v_pastes_clipboard() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    clipboard.set_text("pasted").unwrap();
    let mut core = basic_core_with_clipboard(clipboard.clone());
    core.handle_key(ctrl('v')).await;
    assert_eq!(buffer_text(&core), "pasted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_z_undoes_last_insert() {
    let mut core = basic_core();
    for c in ['a', 'b'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('z')).await;
    assert_eq!(buffer_text(&core), "a");
    core.handle_key(ctrl('z')).await;
    assert_eq!(buffer_text(&core), "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_shift_z_redoes_previous_undo() {
    let mut core = basic_core();
    for c in ['a', 'b'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('z')).await;
    assert_eq!(buffer_text(&core), "a");
    core.handle_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .await;
    assert_eq!(buffer_text(&core), "ab");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typing_over_selection_replaces_it() {
    let mut core = basic_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(plain('X')).await;
    assert_eq!(buffer_text(&core), "X");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_clears_selection_without_mutating_buffer() {
    let mut core = basic_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    assert!(core.editor().has_selection());
    core.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)).await;
    assert!(!core.editor().has_selection());
    assert_eq!(buffer_text(&core), "abc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_and_end_jump_to_line_extremes() {
    let mut core = basic_core();
    for c in ['h', 'e', 'l', 'l', 'o'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .await;
    core.handle_key(plain('-')).await;
    core.handle_key(key(KeyCode::End, KeyModifiers::NONE)).await;
    core.handle_key(plain('!')).await;
    assert_eq!(buffer_text(&core), "-hello!");
}
