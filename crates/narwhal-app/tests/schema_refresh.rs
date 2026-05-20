//! Integration tests for the `:refresh` command and auto-refresh on DDL.
//!
//! Five tests:
//! 1. `manual_refresh_repopulates_sidebar`
//! 2. `create_table_triggers_auto_refresh`
//! 3. `drop_table_triggers_auto_refresh`
//! 4. `non_ddl_no_refresh`
//! 5. `batched_ddl_debounces`

use std::path::PathBuf;

use narwhal_app::core::AppCore;
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

/// Count tables visible in the session's schema listing.
fn table_count(core: &AppCore) -> usize {
    core.session()
        .map(|s| s.schemas.iter().map(|(_, tables)| tables.len()).sum())
        .unwrap_or(0)
}

/// 1. Manual `:refresh` re-fetches the schema catalogue and reports
///    the table count in the status bar.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_refresh_repopulates_sidebar() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE alpha (id INTEGER PRIMARY KEY);
             CREATE TABLE beta (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    // Initial open should already list both tables.
    assert_eq!(table_count(&core), 2);

    // Add a table behind the scenes (simulating an external process).
    {
        let conn = rusqlite::Connection::open(dir.path().join("test.db")).unwrap();
        conn.execute_batch("CREATE TABLE gamma (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    // Sidebar is stale — still shows 2.
    assert_eq!(table_count(&core), 2);

    // Explicit refresh updates the cache.
    core.execute_command("refresh");
    assert_eq!(table_count(&core), 3);
    assert!(
        core.status_message()
            .contains("schema refreshed · 3 tables"),
        "expected table count in status, got: {}",
        core.status_message()
    );
}

/// 2. CREATE TABLE triggers auto-refresh; sidebar gains the new table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_table_triggers_auto_refresh() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    assert_eq!(table_count(&core), 0);

    core.insert_into_editor("CREATE TABLE new_one (id INTEGER PRIMARY KEY)");
    core.execute_command("run");
    core.drain_run_updates_and_refresh().await;

    assert_eq!(table_count(&core), 1);
}

/// 3. DROP TABLE triggers auto-refresh; sidebar no longer shows the
///    dropped table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_table_triggers_auto_refresh() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE going_away (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    assert_eq!(table_count(&core), 1);

    core.insert_into_editor("DROP TABLE going_away");
    core.execute_command("run");
    core.drain_run_updates_and_refresh().await;

    assert_eq!(table_count(&core), 0);
}

/// 4. Non-DDL statements (SELECT) do not schedule an auto-refresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_ddl_no_refresh() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stuff (id INTEGER PRIMARY KEY, val TEXT);
             INSERT INTO stuff VALUES (1, 'hello');",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    let tables_before = table_count(&core);

    core.insert_into_editor("SELECT * FROM stuff");
    core.execute_command("run");
    core.drain_run_updates().await;

    // No debounce task should have been scheduled.
    assert!(
        core.refresh_task().is_none(),
        "non-DDL should not schedule a refresh"
    );
    assert_eq!(table_count(&core), tables_before);
}

/// 5. A batch with multiple DDL statements fires exactly one refresh
///    (debounced). Three CREATE TABLEs in a single `:run-all` should
///    result in 3 new tables but only one refresh cycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batched_ddl_debounces() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

    assert_eq!(table_count(&core), 0);

    core.insert_into_editor(
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY); \
         CREATE TABLE t2 (id INTEGER PRIMARY KEY); \
         CREATE TABLE t3 (id INTEGER PRIMARY KEY)",
    );
    core.execute_command("run-all");
    core.drain_run_updates_and_refresh().await;

    // All three tables should now be visible.
    assert_eq!(table_count(&core), 3);

    // The status bar should reflect a single refresh (not three).
    assert!(
        core.status_message()
            .contains("schema refreshed · 3 tables"),
        "expected single refreshed status, got: {}",
        core.status_message()
    );
}
