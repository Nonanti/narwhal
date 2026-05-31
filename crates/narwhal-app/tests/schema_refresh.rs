//! Integration tests for the `:refresh` command and auto-refresh on DDL.
//!
//! Seven tests:
//! 1. `manual_refresh_repopulates_sidebar`
//! 2. `create_table_triggers_auto_refresh`
//! 3. `drop_table_triggers_auto_refresh`
//! 4. `non_ddl_no_refresh`
//! 5. `batched_ddl_debounces`
//! 6. `schema_refresh_skipped_when_session_changed` (bug C5)
//! 7. `refresh_uses_list_all_tables` (H12 — single query path)

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

/// Two-connection fixture used by the session-mismatch test (C5).
fn fixture_pair(db_a: PathBuf, db_b: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: vec![
            ConnectionConfig {
                id: Uuid::from_u128(1),
                name: "alpha".into(),
                driver: "sqlite".into(),
                params: ConnectionParams::with(|p| {
                    p.path = Some(db_a.to_string_lossy().into_owned());
                }),
            },
            ConnectionConfig {
                id: Uuid::from_u128(2),
                name: "beta".into(),
                driver: "sqlite".into(),
                params: ConnectionParams::with(|p| {
                    p.path = Some(db_b.to_string_lossy().into_owned());
                }),
            },
        ],
    };
    (registry, connections)
}

/// Count tables visible in the session's schema listing.
fn table_count(core: &AppCore) -> usize {
    core.session().map_or(0, |s| {
        s.schemas.iter().map(|(_, tables)| tables.len()).sum()
    })
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
    core.execute_command("open headless").await;

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
    core.execute_command("refresh").await;
    core.drain_meta_updates().await;
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
    core.execute_command("open headless").await;

    assert_eq!(table_count(&core), 0);

    core.insert_into_editor("CREATE TABLE new_one (id INTEGER PRIMARY KEY)")
        .await;
    core.execute_command("run").await;
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
    core.execute_command("open headless").await;

    assert_eq!(table_count(&core), 1);

    core.insert_into_editor("DROP TABLE going_away").await;
    core.execute_command("run").await;
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
    core.execute_command("open headless").await;

    let tables_before = table_count(&core);

    core.insert_into_editor("SELECT * FROM stuff").await;
    core.execute_command("run").await;
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
    core.execute_command("open headless").await;

    assert_eq!(table_count(&core), 0);

    core.insert_into_editor(
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY); \
         CREATE TABLE t2 (id INTEGER PRIMARY KEY); \
         CREATE TABLE t3 (id INTEGER PRIMARY KEY)",
    )
    .await;
    core.execute_command("run-all").await;
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

/// 6. (C5) DDL on session A schedules a debounced refresh; if the
///    user switches to session B before the timer fires, B must NOT
///    be refreshed on A's behalf. Previously the debounce task fired
///    `RunUpdate::SchemaRefresh` and the handler blindly refreshed
///    `self.session` — which had become B.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_refresh_skipped_when_session_changed() {
    let dir = TempDir::new().unwrap();
    let db_a = dir.path().join("alpha.db");
    let db_b = dir.path().join("beta.db");
    rusqlite::Connection::open(&db_a).unwrap();
    // B has one table from the start; we'll watch the table count
    // to detect any spurious refresh.
    {
        let conn = rusqlite::Connection::open(&db_b).unwrap();
        conn.execute_batch("CREATE TABLE only_b (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    let (registry, connections) = fixture_pair(db_a, db_b);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open alpha").await;
    assert_eq!(table_count(&core), 0);

    // Run a DDL on A and drain through AllDone but not through the
    // debounce timer — the refresh is scheduled but has not yet
    // fired.
    core.insert_into_editor("CREATE TABLE on_alpha (id INTEGER PRIMARY KEY)")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    assert!(
        core.refresh_task().is_some(),
        "DDL on A should have scheduled a refresh"
    );

    // Switch to B *before* the debounce fires.
    core.execute_command("open beta").await;
    let active_id_after_switch = core.session().map(|s| s.config.id);
    assert_eq!(active_id_after_switch, Some(Uuid::from_u128(2)));
    let tables_b_initial = table_count(&core);
    assert_eq!(tables_b_initial, 1, "B starts with one table");

    let status_before = core.status_message().to_owned();

    // Let the debounce fire and the stale SchemaRefresh arrive.
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    while let Some(update) = core.try_recv_run_update().await {
        core.handle_run_update(update).await;
    }

    // B's table count must be unchanged — the refresh targeted A and
    // should have been suppressed.
    assert_eq!(
        table_count(&core),
        1,
        "B's schema was refreshed on A's behalf"
    );
    // And the status must not advertise a fresh refresh.
    assert!(
        !core.status_message().contains("schema refreshed"),
        "expected no \"schema refreshed\" status; was: {} (before: {})",
        core.status_message(),
        status_before
    );
}

/// 7. (H12) `Session::refresh_schemas` now uses `Connection::list_all_tables`
///    instead of the N+1 `list_schemas` + per-schema `list_tables` pattern.
///    Verify end-to-end by opening a connection with multiple tables
///    and ensuring refresh still works correctly (the default impl of
///    `list_all_tables` falls back to N+1, but the call path is
///    through `list_all_tables`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_uses_list_all_tables() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("h12.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE one (id INTEGER PRIMARY KEY);
             CREATE TABLE two (id INTEGER PRIMARY KEY);
             CREATE TABLE three (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;

    // Initial refresh should see all 3 tables.
    assert_eq!(table_count(&core), 3);

    // Add a table externally.
    {
        let conn = rusqlite::Connection::open(dir.path().join("h12.db")).unwrap();
        conn.execute_batch("CREATE TABLE four (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    // Manual refresh via the new list_all_tables path.
    core.execute_command("refresh").await;
    core.drain_meta_updates().await;
    assert_eq!(table_count(&core), 4);
    assert!(
        core.status_message()
            .contains("schema refreshed · 4 tables"),
        "expected table count in status, got: {}",
        core.status_message()
    );
}
