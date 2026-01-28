//! Integration tests for the row detail modal (plan 07-02).
//!
//! Each test creates an `AppCore` with an in-memory `SQLite` session,
//! seeds a result set, then drives key events to exercise the row
//! detail open, navigation, and dismiss flows.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
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
            name: "row-detail-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
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

const fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Focus the result pane via Ctrl-W cycling.
fn focus_results(core: &mut AppCore) {
    let ctrl_w = ctrl(KeyCode::Char('w'));
    while core.focus() != Pane::Results {
        core.handle_key(ctrl_w);
    }
}

/// Seed a 3-row result set with (id INT, label TEXT) and focus results.
async fn seed_result(core: &mut AppCore, db_path: PathBuf) {
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items VALUES (1, 'alpha');
             INSERT INTO items VALUES (2, 'beta');
             INSERT INTO items VALUES (3, 'gamma');",
        )
        .unwrap();
    }
    core.execute_command("open row-detail-test");
    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;
    focus_results(core);
    // Select the first row (j selects row 0 if nothing is selected).
    core.handle_key(key(KeyCode::Char('j')));
}

// Test 1: open_with_no_row_shows_status_message

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_with_no_row_shows_status_message() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open row-detail-test");
    // No query run — no results, no selection.
    focus_results(&mut core);

    // Press R to try opening row detail with no selection.
    core.handle_key(key(KeyCode::Char('R')));

    assert!(
        !core.row_detail_is_open(),
        "row detail should not open without a selected row"
    );
    assert!(
        core.status_message().contains("no row selected"),
        "expected status message about no row selected, got: {}",
        core.status_message()
    );
}

// Test 2: open_populates_columns_and_values

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_populates_columns_and_values() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;

    // Press R to open row detail.
    core.handle_key(key(KeyCode::Char('R')));

    assert!(core.row_detail_is_open(), "row detail should be open");

    let state = core
        .row_detail_state()
        .expect("row detail state should exist");
    assert_eq!(state.columns.len(), 2, "should have 2 columns (id, label)");
    assert_eq!(
        state.values.len(),
        2,
        "should have 2 values matching columns"
    );
    assert_eq!(
        state.selected_column, 0,
        "selected column should start at 0"
    );
}

// Test 3: navigate_selects_columns

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigate_selects_columns() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;

    // Open row detail.
    core.handle_key(key(KeyCode::Char('R')));
    assert!(core.row_detail_is_open());

    // Press j twice to navigate down.
    core.handle_key(key(KeyCode::Char('j')));
    core.handle_key(key(KeyCode::Char('j')));

    let state = core
        .row_detail_state()
        .expect("row detail state should exist");
    assert_eq!(
        state.selected_column, 1,
        "after pressing j twice, selected_column should be 1 (clamped to last column)"
    );

    // Press k to go back up.
    core.handle_key(key(KeyCode::Char('k')));
    let state = core
        .row_detail_state()
        .expect("row detail state should exist");
    assert_eq!(
        state.selected_column, 0,
        "after pressing k, selected_column should be 0"
    );
}

// Test 4: esc_closes

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_closes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;

    // Open row detail.
    core.handle_key(key(KeyCode::Char('R')));
    assert!(core.row_detail_is_open(), "row detail should be open");

    // Press Esc to dismiss.
    core.handle_key(key(KeyCode::Esc));

    assert!(
        !core.row_detail_is_open(),
        "row detail should be closed after Esc"
    );
    assert!(
        core.status_message().contains("row detail closed"),
        "expected status message about closing, got: {}",
        core.status_message()
    );
}
