//! Integration tests for the inline cell edit flow.
//!
//! Each scenario:
//!   1. seeds a sqlite database,
//!   2. opens it through `AppCore` and previews a table from the sidebar
//!      (which is the only path that attaches a `RowSource` right now),
//!   3. simulates the `e` keystroke + buffer edit + Enter,
//!   4. verifies the on-disk row matches the new value.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams, Value};
use narwhal_tui::Pane;
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

const fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

async fn type_str(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
}

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "edit".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

/// Move focus to the sidebar, take `jumps` steps with `j`, and trigger a
/// preview with `o`. Drains the resulting run updates.
async fn preview_at(core: &mut AppCore, jumps: usize) {
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl('w')).await;
    }
    for _ in 0..jumps {
        core.handle_key(key(KeyCode::Char('j'))).await;
    }
    core.handle_key(key(KeyCode::Char('o'))).await;
    core.drain_run_updates().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_string_cell_updates_database() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("e.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('alpha'), ('beta');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open edit").await;
    // Sidebar layout: connection (0) -> main schema (1) -> items (2).
    preview_at(&mut core, 2).await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w')).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('l'))).await; // column 1 = label

    core.handle_key(key(KeyCode::Char('e'))).await;
    for _ in 0..16 {
        core.handle_key(key(KeyCode::Backspace)).await;
    }
    type_str(&mut core, "gamma").await;
    core.handle_key(key(KeyCode::Enter)).await;

    // L36: the edit is now staged. The in-memory grid reflects the
    // queued value, but the database has not been touched yet.
    assert!(
        core.status_message().contains("queued UPDATE")
            && core.status_message().contains("1 pending"),
        "got status: {}",
        core.status_message()
    );
    match core.result() {
        ResultState::Rows { rows, .. } => match &rows[0].0[1] {
            Value::String(s) => assert_eq!(s, "gamma"),
            other => panic!("expected updated string, got {other:?}"),
        },
        other => panic!("expected Rows, got {other:?}"),
    }

    // Database still holds the original value until Ctrl-S.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let label: String = conn
            .query_row("SELECT label FROM items WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label, "alpha", "DB must not change before Ctrl-S");
    }

    // Ctrl-S commits the pending queue. `commit_pending` runs the
    // transaction synchronously and then kicks off an async preview
    // refresh; the status bar passes through "committed"
    // → "executing…" → "done" by the time we observe it, so we
    // verify the commit by inspecting the database directly
    // (transactional truth) rather than racing the status bar.
    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let label: String = conn
        .query_row("SELECT label FROM items WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(label, "gamma");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_with_null_token_sets_null() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("e.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('alpha');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open edit").await;
    preview_at(&mut core, 2).await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w')).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('l'))).await;

    core.handle_key(key(KeyCode::Char('e'))).await;
    for _ in 0..16 {
        core.handle_key(key(KeyCode::Backspace)).await;
    }
    type_str(&mut core, "NULL").await;
    core.handle_key(key(KeyCode::Enter)).await;
    assert!(
        core.status_message().contains("queued UPDATE"),
        "got: {}",
        core.status_message()
    );
    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let null: Option<String> = conn
        .query_row("SELECT label FROM items WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert!(null.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_rejected_for_table_without_pk() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("e.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // No declared primary key.
        conn.execute_batch("CREATE TABLE notes (n TEXT); INSERT INTO notes VALUES ('x');")
            .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open edit").await;
    preview_at(&mut core, 2).await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w')).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('e'))).await;
    let msg = core.status_message();
    assert!(
        msg.contains("no primary key") || msg.contains("disabled") || msg.contains("read-only"),
        "got status: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freeform_run_results_are_read_only() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("e.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('alpha');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open edit").await;

    core.insert_into_editor("SELECT id, label FROM items").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w')).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('e'))).await;
    assert!(core.status_message().contains("read-only"));
}
