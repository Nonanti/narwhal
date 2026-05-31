//! Integration tests for the L36 JSON viewer modal.
//!
//! Exercises the open/close path from both the result pane (`z` over
//! a cell) and the row-detail modal (`Z` over the focused column),
//! plus the scroll/yank chords.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::clipboard::InMemoryClipboard;
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::{ConnectionsFile, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::Pane;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "headless".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

const fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

async fn open_with_payload(payload: &str) -> (AppCore, Arc<InMemoryClipboard>) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("j.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE blobs (id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO blobs (id, body) VALUES (1, '{}');",
        payload.replace('\'', "''")
    ))
    .unwrap();
    drop(conn);
    // Leak the dir so the file outlives the function. Tests are short-lived
    // and we don't care about cleanup.
    std::mem::forget(dir);

    let (registry, connections) = fixture(db_path);
    let clipboard = Arc::new(InMemoryClipboard::new());
    let mut core = AppCore::with_services(
        registry,
        connections,
        None,
        Arc::new(InMemoryStore::new()),
        clipboard.clone(),
    );
    core.execute_command("open headless").await;
    core.insert_into_editor("SELECT id, body FROM blobs").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    // Hand focus to the results pane and select the first row.
    let ctrl_w = ctrl('w');
    while core.focus() != Pane::Results {
        core.handle_key(ctrl_w).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await; // select row 0
                                                    // Move to the `body` column.
    core.handle_key(key(KeyCode::Char('l'))).await;
    (core, clipboard)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn z_opens_json_viewer_with_pretty_text() {
    let (mut core, _cb) = open_with_payload(r#"{"a":1,"b":[2,3]}"#).await;
    core.handle_key(key(KeyCode::Char('z'))).await;

    let view = core.json_viewer_for_test().expect("modal must open");
    assert!(view.parse_error.is_none(), "valid JSON parses cleanly");
    assert!(view.pretty.contains("\"a\""));
    // Pretty-print adds newlines; the raw payload was a single line.
    assert!(view.pretty.lines().count() > 1, "must pretty-print");
    assert_eq!(view.scroll, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_json_falls_back_to_raw_with_error() {
    let (mut core, _cb) = open_with_payload("not json {").await;
    core.handle_key(key(KeyCode::Char('z'))).await;

    let view = core.json_viewer_for_test().expect("modal must open");
    assert!(view.parse_error.is_some(), "invalid JSON surfaces error");
    assert_eq!(view.pretty, "not json {");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scroll_chords_advance_and_clamp() {
    // Multi-line payload large enough to scroll.
    let payload = format!(
        "{{\"items\":[{}]}}",
        (0..20)
            .map(|i| format!("{{\"i\":{i}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let (mut core, _cb) = open_with_payload(&payload).await;
    core.handle_key(key(KeyCode::Char('z'))).await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    assert_eq!(core.json_viewer_for_test().unwrap().scroll, 2);

    core.handle_key(key(KeyCode::Char('k'))).await;
    assert_eq!(core.json_viewer_for_test().unwrap().scroll, 1);

    core.handle_key(ctrl('d')).await;
    assert_eq!(core.json_viewer_for_test().unwrap().scroll, 11);

    core.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(core.json_viewer_for_test().unwrap().scroll, 0);

    core.handle_key(key(KeyCode::Char('G'))).await;
    // G clamps to the last line index.
    let total = core.json_viewer_for_test().unwrap().pretty.lines().count() as u16;
    assert_eq!(
        core.json_viewer_for_test().unwrap().scroll,
        total.saturating_sub(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yank_copies_pretty_and_raw_variants() {
    let (mut core, clipboard) = open_with_payload(r#"{"x":1}"#).await;
    core.handle_key(key(KeyCode::Char('z'))).await;

    core.handle_key(key(KeyCode::Char('y'))).await;
    let pretty = clipboard.read().expect("clipboard write expected");
    assert!(pretty.contains("\"x\": 1") || pretty.contains("\"x\":1"));
    assert!(
        pretty.contains('\n'),
        "yank `y` must copy the pretty (multi-line) form"
    );

    core.handle_key(KeyEvent {
        code: KeyCode::Char('Y'),
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
    .await;
    let raw = clipboard.read().expect("clipboard write expected");
    assert_eq!(raw, r#"{"x":1}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_and_q_dismiss_the_viewer() {
    let (mut core, _cb) = open_with_payload(r#"{"x":1}"#).await;
    core.handle_key(key(KeyCode::Char('z'))).await;
    assert!(core.json_viewer_for_test().is_some());
    core.handle_key(key(KeyCode::Esc)).await;
    assert!(core.json_viewer_for_test().is_none());

    // Re-open and dismiss with `q`.
    core.handle_key(key(KeyCode::Char('z'))).await;
    assert!(core.json_viewer_for_test().is_some());
    core.handle_key(key(KeyCode::Char('q'))).await;
    assert!(core.json_viewer_for_test().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shift_z_opens_viewer_from_row_detail_modal() {
    let (mut core, _cb) = open_with_payload(r#"{"row":42}"#).await;
    // Open the row detail modal first (Shift+Enter).
    core.handle_key(KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
    .await;
    // Step the row-detail cursor down to the `body` column (index 1).
    core.handle_key(key(KeyCode::Char('j'))).await;
    // Now Shift+Z to launch the JSON viewer over `body`.
    core.handle_key(KeyEvent {
        code: KeyCode::Char('Z'),
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
    .await;
    let view = core
        .json_viewer_for_test()
        .expect("modal must open from row-detail");
    assert!(view.pretty.contains("42"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_or_empty_cell_does_not_open_modal() {
    // Seed a NULL body and confirm the viewer never opens.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("j.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE blobs (id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO blobs (id, body) VALUES (1, NULL);",
    )
    .unwrap();
    drop(conn);

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;
    core.insert_into_editor("SELECT id, body FROM blobs").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    let ctrl_w = ctrl('w');
    while core.focus() != Pane::Results {
        core.handle_key(ctrl_w).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('z'))).await;

    assert!(
        core.json_viewer_for_test().is_none(),
        "NULL cell must not open the viewer"
    );
    assert!(core.status_message().contains("NULL") || core.status_message().contains("empty"));
}
