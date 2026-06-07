//! Integration tests for the emacs editor handler.
//!
//! Active when `[editor].mode = "emacs"`. Covers the classic Ctrl /
//! Meta chord set: motions, set-mark / region kill, yank, kill-line,
//! undo, and the two-stroke `C-x` prefix.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::clipboard::{Clipboard, InMemoryClipboard};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
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

const fn alt(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::ALT)
}

fn emacs_core_with_clipboard(clipboard: Arc<dyn Clipboard>) -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_services(
        registry,
        ConnectionsFile::default(),
        None,
        Arc::new(InMemoryStore::new()),
        clipboard,
    );
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Emacs;
    core.apply_settings(settings);
    core
}

fn emacs_core() -> AppCore {
    emacs_core_with_clipboard(Arc::new(InMemoryClipboard::new()))
}

fn buffer_text(core: &AppCore) -> String {
    core.editor().entire_text()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_typing_inserts_characters() {
    let mut core = emacs_core();
    for c in ['h', 'i'] {
        core.handle_key(plain(c)).await;
    }
    assert_eq!(buffer_text(&core), "hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_f_moves_right_c_b_moves_left() {
    let mut core = emacs_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('b')).await;
    core.handle_key(ctrl('b')).await;
    core.handle_key(plain('X')).await;
    assert_eq!(buffer_text(&core), "aXbc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_a_jumps_to_line_start_and_c_e_to_end() {
    let mut core = emacs_core();
    for c in ['h', 'e', 'l', 'l', 'o'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(plain('-')).await;
    core.handle_key(ctrl('e')).await;
    core.handle_key(plain('!')).await;
    assert_eq!(buffer_text(&core), "-hello!");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_space_sets_mark_and_motion_extends_region() {
    let mut core = emacs_core();
    for c in ['a', 'b', 'c', 'd', 'e'] {
        core.handle_key(plain(c)).await;
    }
    // Back two chars then set the mark, then move forward two so the
    // selection covers two chars.
    core.handle_key(ctrl('b')).await;
    core.handle_key(ctrl('b')).await;
    core.handle_key(ctrl(' ')).await;
    core.handle_key(ctrl('f')).await;
    core.handle_key(ctrl('f')).await;
    assert_eq!(core.editor().selected_text(), "de");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m_w_copies_region_to_clipboard() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = emacs_core_with_clipboard(clipboard.clone());
    for c in ['c', 'o', 'p', 'y'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await; // move to BOL
    core.handle_key(ctrl(' ')).await; // set mark
    core.handle_key(ctrl('e')).await; // extend to EOL
    core.handle_key(alt('w')).await;
    assert_eq!(clipboard.read().as_deref(), Some("copy"));
    // M-w keeps the buffer intact.
    assert_eq!(buffer_text(&core), "copy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_w_kills_region_to_clipboard() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = emacs_core_with_clipboard(clipboard.clone());
    for c in ['c', 'u', 't'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(ctrl(' ')).await;
    core.handle_key(ctrl('e')).await;
    core.handle_key(ctrl('w')).await;
    assert_eq!(clipboard.read().as_deref(), Some("cut"));
    assert_eq!(buffer_text(&core), "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_y_yanks_clipboard_contents() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    clipboard.set_text("yanked").unwrap();
    let mut core = emacs_core_with_clipboard(clipboard);
    core.handle_key(ctrl('y')).await;
    assert_eq!(buffer_text(&core), "yanked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_k_kills_to_end_of_line() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = emacs_core_with_clipboard(clipboard.clone());
    for c in ['h', 'e', 'l', 'l', 'o'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(ctrl('f')).await;
    core.handle_key(ctrl('f')).await;
    core.handle_key(ctrl('k')).await;
    assert_eq!(buffer_text(&core), "he");
    assert_eq!(clipboard.read().as_deref(), Some("llo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_slash_undoes_last_edit() {
    let mut core = emacs_core();
    for c in ['a', 'b'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('/')).await;
    assert_eq!(buffer_text(&core), "a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_g_clears_region_without_mutating_buffer() {
    let mut core = emacs_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(ctrl('a')).await;
    core.handle_key(ctrl(' ')).await;
    core.handle_key(ctrl('e')).await;
    assert!(core.editor().has_selection());
    core.handle_key(ctrl('g')).await;
    assert!(!core.editor().has_selection());
    assert_eq!(buffer_text(&core), "abc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c_x_prefix_consumes_one_chord() {
    // C-x without a follow-up just leaves the prefix flag set and a
    // status hint; an unbound follow-up clears it cleanly without
    // wedging the editor.
    let mut core = emacs_core();
    core.handle_key(ctrl('x')).await;
    core.handle_key(plain('q')).await; // unbound after C-x
    // Buffer remains empty; subsequent typing should land normally.
    core.handle_key(plain('z')).await;
    assert_eq!(buffer_text(&core), "z");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arrow_keys_move_cursor() {
    let mut core = emacs_core();
    for c in ['a', 'b', 'c'] {
        core.handle_key(plain(c)).await;
    }
    core.handle_key(key(KeyCode::Left, KeyModifiers::NONE)).await;
    core.handle_key(plain('X')).await;
    assert_eq!(buffer_text(&core), "abXc");
}
