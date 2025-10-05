//! Integration tests that exercise [`AppCore`] without a terminal.
//!
//! The tests open an in-memory SQLite session, dispatch commands and
//! verify the resulting [`ResultState`].

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::Pane;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "headless".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_then_run_returns_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('first'), ('second'), ('third');",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    assert!(matches!(core.result(), ResultState::Empty));
    assert!(core.session().is_none());

    core.execute_command("open headless");
    assert!(core.session().is_some(), "session must open");

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { columns, rows, .. } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(rows.len(), 3);
        }
        other => panic!("expected Rows, got {other:?}"),
    }
    assert!(core.status_message().contains("done · 1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_all_executes_every_statement() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open headless");
    core.insert_into_editor(
        "CREATE TABLE notes (n TEXT); \
         INSERT INTO notes VALUES ('alpha'); \
         INSERT INTO notes VALUES ('beta'); \
         SELECT * FROM notes",
    );
    core.execute_command("run-all");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected Rows, got {other:?}"),
    }
    assert!(core.status_message().contains("done · 4"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_connection_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open does-not-exist");
    assert!(core.session().is_none());
    assert!(core.status_message().contains("connection not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_enter_opens_table_detail() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("detail.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE);
             CREATE TABLE orders (
                 id INTEGER PRIMARY KEY,
                 customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
                 placed_at TEXT NOT NULL
             );",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    // Switch focus to the sidebar (Ctrl-W cycles editor->results->sidebar).
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl_w);
    }
    // Step down to the `orders` row: connection (0) -> main (1) ->
    // customers (2) -> orders (3).
    let j = KeyEvent {
        code: KeyCode::Char('j'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    for _ in 0..3 {
        core.handle_key(j);
    }
    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(enter);

    match core.result() {
        ResultState::TableDetail { schema } => {
            assert_eq!(schema.table.name, "orders");
            assert_eq!(schema.columns.len(), 3);
            assert_eq!(schema.foreign_keys.len(), 1);
            assert_eq!(schema.foreign_keys[0].referenced_table, "customers");
        }
        other => panic!("expected TableDetail, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_without_session_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("run");
    assert!(core.status_message().contains("no active connection"));
    assert!(matches!(core.result(), ResultState::Empty));
}
