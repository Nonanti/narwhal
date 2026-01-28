//! Integration tests for multi-statement output tabs (plan 07-03).
//!
//! Each test creates an `AppCore` with an in-memory `SQLite` session,
//! dispatches multi-statement batches, and verifies that the
//! `ResultBundle` correctly holds one entry per statement.

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
            name: "multi-stmt-test".into(),
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

// Test 1: single_result_no_strip

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_result_no_strip() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open multi-stmt-test");

    core.insert_into_editor("SELECT * FROM t");
    core.execute_command("run");
    core.drain_run_updates().await;

    let bundle = core.tabs()[core.active_tab()].results();
    assert_eq!(bundle.len(), 1, "single result should have bundle length 1");
    assert!(!bundle.is_multi(), "single result should not be multi");
    assert!(matches!(bundle.active_state(), ResultState::Rows { .. }));
}

// Test 2: three_statements_three_results

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_statements_three_results() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO t VALUES (1, 'a');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open multi-stmt-test");

    core.insert_into_editor("SELECT 1; SELECT 2; SELECT 3;");
    core.execute_command("run-all");
    core.drain_run_updates().await;

    let bundle = core.tabs()[core.active_tab()].results();
    assert_eq!(
        bundle.len(),
        3,
        "three statements should produce three results"
    );
    assert!(bundle.is_multi(), "three results should be multi");
    assert_eq!(bundle.active, 2, "active defaults to last result");
}

// Test 3: ]r_advances_active

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bracket_r_advances_active() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open multi-stmt-test");

    core.insert_into_editor("SELECT 1; SELECT 2; SELECT 3;");
    core.execute_command("run-all");
    core.drain_run_updates().await;
    focus_results(&mut core);

    // ] then r — active starts at 2 (last), wraps to 0
    core.handle_key(key(KeyCode::Char(']')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        0,
        "]r should wrap to result 0"
    );

    // ]r again → 1
    core.handle_key(key(KeyCode::Char(']')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        1,
        "]r should advance to result 1"
    );

    // ]r again → 2
    core.handle_key(key(KeyCode::Char(']')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        2,
        "]r should advance to result 2"
    );
}

// Test 4: [r_wraps_backward

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bracket_l_bracket_r_wraps_backward() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let _conn = rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open multi-stmt-test");

    core.insert_into_editor("SELECT 1; SELECT 2; SELECT 3;");
    core.execute_command("run-all");
    core.drain_run_updates().await;
    focus_results(&mut core);

    // [ then r — from active 2 should go to 1
    core.handle_key(key(KeyCode::Char('[')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        1,
        "[r should go to result 1"
    );

    // [r again → 0
    core.handle_key(key(KeyCode::Char('[')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        0,
        "[r should go to result 0"
    );

    // [r wraps → 2
    core.handle_key(key(KeyCode::Char('[')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        2,
        "[r should wrap backward to result 2"
    );
}

// Test 5: state_preserved_across_tab_switch

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_preserved_across_tab_switch() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
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
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open multi-stmt-test");

    core.insert_into_editor("SELECT * FROM items; SELECT 42;");
    core.execute_command("run-all");
    core.drain_run_updates().await;
    focus_results(&mut core);

    // Active starts at result 1 (last). Switch to result 0 first.
    core.handle_key(key(KeyCode::Char('[')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        0,
        "should be on result 0"
    );

    // Move down three times: None -> 0 -> 1 -> 2
    core.handle_key(key(KeyCode::Char('j')));
    core.handle_key(key(KeyCode::Char('j')));
    core.handle_key(key(KeyCode::Char('j')));
    let first_result_selected = core.tabs()[core.active_tab()].results().active().selected();
    assert_eq!(
        first_result_selected,
        Some(2),
        "should be on row 2 in result 0"
    );

    // Switch to result 1
    core.handle_key(key(KeyCode::Char(']')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        1,
        "should be on result 1"
    );

    // Switch back to result 0
    core.handle_key(key(KeyCode::Char('[')));
    core.handle_key(key(KeyCode::Char('r')));
    assert_eq!(
        core.tabs()[core.active_tab()].results().active,
        0,
        "should be back on result 0"
    );

    // Verify scroll state was preserved
    let restored_selected = core.tabs()[core.active_tab()].results().active().selected();
    assert_eq!(
        restored_selected,
        Some(2),
        "scroll state should be preserved across tab switch"
    );
}
