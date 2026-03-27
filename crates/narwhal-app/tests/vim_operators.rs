//! Integration tests for M3.1: vim operator wiring (yank / delete / change).
//!
//! Exercises the full dispatch path: crossterm key → vim state machine →
//! `apply_action` → `EditorBuffer` mutation + clipboard write.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::clipboard::{Clipboard, InMemoryClipboard};
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, DynCredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Build a fresh `AppCore` with an in-memory clipboard. No database
/// session is needed — we only exercise the editor buffer.
fn core_with_clipboard() -> (AppCore, Arc<InMemoryClipboard>) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("vo.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "vo".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };
    let creds: Arc<dyn DynCredentialStore> = Arc::new(InMemoryStore::new());
    let clip = Arc::new(InMemoryClipboard::new());
    let clip_dyn: Arc<dyn Clipboard> = clip.clone();
    let core = AppCore::with_services(registry, connections, None, creds, clip_dyn);
    (core, clip)
}

/// Seed the editor buffer with text by typing it in insert mode.
async fn type_text(core: &mut AppCore, text: &str) {
    // Enter insert mode
    core.handle_key(key(KeyCode::Char('i'))).await;
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
    // Return to normal mode
    core.handle_key(key(KeyCode::Esc)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dw_deletes_word() {
    let (mut core, clip) = core_with_clipboard();
    type_text(&mut core, "hello world").await;
    // Move cursor to start of line
    core.handle_key(key(KeyCode::Char('0'))).await;
    // d → operator pending, w → word forward
    core.handle_key(key(KeyCode::Char('d'))).await;
    core.handle_key(key(KeyCode::Char('w'))).await;
    let editor = core.editor();
    assert_eq!(editor.lines(), &["world"]);
    // Clipboard should contain the deleted text
    assert!(clip.read().is_some_and(|t| t.contains("hello")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dd_deletes_line() {
    let (mut core, clip) = core_with_clipboard();
    type_text(&mut core, "line0\nline1\nline2").await;
    // dd → delete current line (cursor should be on line2 after typing,
    // so move up first)
    core.handle_key(key(KeyCode::Char('k'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    let editor = core.editor();
    assert_eq!(editor.lines(), &["line0", "line2"]);
    assert!(clip.read().is_some_and(|t| t.contains("line1")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yy_yanks_line() {
    let (mut core, clip) = core_with_clipboard();
    type_text(&mut core, "line0\nline1\nline2").await;
    // Move to line1
    core.handle_key(key(KeyCode::Char('k'))).await;
    // yy → yank current line
    core.handle_key(key(KeyCode::Char('y'))).await;
    core.handle_key(key(KeyCode::Char('y'))).await;
    // Buffer should NOT change
    let editor = core.editor();
    assert_eq!(editor.lines(), &["line0", "line1", "line2"]);
    assert!(clip.read().is_some_and(|t| t.contains("line1")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yw_yanks_word() {
    let (mut core, clip) = core_with_clipboard();
    type_text(&mut core, "foo bar baz").await;
    // Move to start
    core.handle_key(key(KeyCode::Char('0'))).await;
    // yw → yank word forward
    core.handle_key(key(KeyCode::Char('y'))).await;
    core.handle_key(key(KeyCode::Char('w'))).await;
    // Buffer should NOT change
    let editor = core.editor();
    assert_eq!(editor.lines(), &["foo bar baz"]);
    let yanked = clip.read().expect("clipboard should have yanked text");
    assert!(
        yanked.contains("foo"),
        "yanked text should contain 'foo', got: {yanked}"
    );
}
