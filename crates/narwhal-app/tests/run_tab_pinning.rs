//! Regression tests for K1-A: run updates must land on the tab that
//! *started* the dispatch, even if the user switches tabs mid-run.
//!
//! Also covers the tab-switch / tab-close guards that prevent
//! `active_tab` from moving while a query is in-flight.

use std::path::PathBuf;

use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "pin-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

/// Multi-statement dispatch on tab 0, then switch to tab 1 —
/// results must still land on tab 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn results_land_on_originating_tab_after_switch() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("pin.db");
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO t VALUES (1, 'alpha');
             INSERT INTO t VALUES (2, 'beta');",
        )
        .expect("seed");
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open pin-test").await;

    // Open a second tab so we have somewhere to switch to.
    core.execute_command("new").await;
    assert_eq!(core.tabs().len(), 2);
    assert_eq!(core.active_tab(), 1, "new tab should be active");

    // Switch back to tab 0 and start a multi-statement batch.
    core.execute_command("tabprev").await;
    assert_eq!(core.active_tab(), 0);
    core.insert_into_editor("SELECT * FROM t; SELECT 42;").await;
    core.execute_command("run-all").await;

    // Drain all updates — results must land on tab 0 regardless.
    core.drain_run_updates().await;
    assert!(!core.is_running(), "run should have completed");

    // Tab 0 should own the result bundle.
    let bundle = core.tabs()[0].results();
    assert_eq!(bundle.len(), 2, "tab 0 should have 2 results");
    match &bundle.states[0] {
        ResultState::Rows { rows, .. } => {
            assert_eq!(rows.len(), 2, "first result should have 2 rows");
        }
        other => panic!("tab 0 result 0: expected Rows, got {other:?}"),
    }

    // Tab 1 should still be empty (untouched).
    let tab1 = core.tabs()[1].results();
    assert!(
        matches!(tab1.active_state(), ResultState::Empty),
        "tab 1 should remain Empty, got {:?}",
        tab1.active_state()
    );
}

/// `cycle_tab` is blocked while a query is running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycle_tab_blocked_while_running() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("guard.db");
    {
        rusqlite::Connection::open(&db_path).expect("open sqlite");
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open pin-test").await;
    core.execute_command("new").await;
    core.execute_command("tabprev").await;
    assert_eq!(core.active_tab(), 0);

    core.insert_into_editor("SELECT 1").await;
    core.execute_command("run").await;

    // Before draining, the run is in-flight.
    assert!(core.is_running());

    // Attempt to switch tabs — should be no-op with status message.
    core.execute_command("tabnext").await;
    assert_eq!(core.active_tab(), 0, "tab switch should be blocked");
    assert!(
        core.status_message().contains("running"),
        "status should explain why tab switch was blocked"
    );

    // Drain and verify run completed normally.
    core.drain_run_updates().await;
    assert!(!core.is_running());
}

/// `close_tab` is blocked while a query is running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_tab_blocked_while_running() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("guard2.db");
    {
        rusqlite::Connection::open(&db_path).expect("open sqlite");
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open pin-test").await;
    core.execute_command("new").await;
    core.execute_command("tabprev").await;

    core.insert_into_editor("SELECT 1").await;
    core.execute_command("run").await;
    assert!(core.is_running());

    core.execute_command("tabclose").await;
    assert_eq!(core.tabs().len(), 2, "tab close should be blocked");
    assert!(
        core.status_message().contains("running"),
        "status should explain why tab close was blocked"
    );

    core.drain_run_updates().await;
}

/// `new_tab` is blocked while a query is running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_tab_blocked_while_running() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("guard3.db");
    {
        rusqlite::Connection::open(&db_path).expect("open sqlite");
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open pin-test").await;

    core.insert_into_editor("SELECT 1").await;
    core.execute_command("run").await;
    assert!(core.is_running());

    core.execute_command("new").await;
    assert_eq!(core.tabs().len(), 1, "new tab should be blocked");
    assert!(
        core.status_message().contains("running"),
        "status should explain why new tab was blocked"
    );

    core.drain_run_updates().await;
}
