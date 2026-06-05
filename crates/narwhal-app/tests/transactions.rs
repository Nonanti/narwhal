//! Integration tests for the `:begin` / `:commit` / `:rollback` /
//! `:savepoint` / `:release` / `:rollback-to` command surface.

use std::path::PathBuf;

use narwhal_app::DriverRegistry;
use narwhal_app::core::{AppCore, ResultState};
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "tx".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

async fn open(db_path: PathBuf) -> AppCore {
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open tx").await;
    core
}

fn seed(db_path: &PathBuf) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch("CREATE TABLE k (v INTEGER); INSERT INTO k VALUES (1);")
        .unwrap();
}

async fn rows_count(core: &mut AppCore) -> i64 {
    core.insert_into_editor("SELECT count(*) AS n FROM k").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    let count = match core.result() {
        ResultState::Rows { rows, .. } => match &rows[0].0[0] {
            narwhal_core::Value::Int(n) => *n,
            other => panic!("expected integer count, got {other:?}"),
        },
        other => panic!("expected Rows, got {other:?}"),
    };
    // Clear the editor for the next round.
    core.execute_command("clear").await;
    count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_reverts_inserts() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("tx.db");
    seed(&db_path);
    let mut core = open(db_path).await;

    assert_eq!(rows_count(&mut core).await, 1);

    core.execute_command("begin").await;
    assert!(core.status_message().contains("transaction started"));

    core.insert_into_editor("INSERT INTO k VALUES (2); INSERT INTO k VALUES (3)")
        .await;
    core.execute_command("run-all").await;
    core.drain_run_updates().await;
    core.execute_command("clear").await;

    // Visible inside the txn.
    assert_eq!(rows_count(&mut core).await, 3);

    core.execute_command("rollback").await;
    core.drain_run_updates().await;
    assert!(core.status_message().contains("rolled back"));
    assert_eq!(rows_count(&mut core).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_persists_inserts() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("tx.db");
    seed(&db_path);
    let mut core = open(db_path).await;

    core.execute_command("begin").await;
    core.insert_into_editor("INSERT INTO k VALUES (2)").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    core.execute_command("clear").await;

    core.execute_command("commit").await;
    // commit dispatches through the run channel; drain before reading
    // the status message so the assertion isn't a race on slow CI
    // runners. (Was flaking on ubuntu-latest after the patch-updates
    // bump landed; reproduces on multi_thread runtime under load.)
    core.drain_run_updates().await;
    assert!(core.status_message().contains("committed"));
    assert_eq!(rows_count(&mut core).await, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn savepoint_then_rollback_to_keeps_outer_changes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("tx.db");
    seed(&db_path);
    let mut core = open(db_path).await;

    core.execute_command("begin").await;

    core.insert_into_editor("INSERT INTO k VALUES (10)").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    core.execute_command("clear").await;

    core.execute_command("savepoint sp1").await;
    assert!(core.status_message().contains("savepoint 'sp1'"));

    core.insert_into_editor("INSERT INTO k VALUES (20); INSERT INTO k VALUES (30)")
        .await;
    core.execute_command("run-all").await;
    core.drain_run_updates().await;
    core.execute_command("clear").await;

    assert_eq!(rows_count(&mut core).await, 4);

    core.execute_command("rollback-to sp1").await;
    assert!(core.status_message().contains("rolled back to savepoint"));

    // The two inner inserts are gone but the outer insert remains.
    assert_eq!(rows_count(&mut core).await, 2);

    core.execute_command("commit").await;
    assert_eq!(rows_count(&mut core).await, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn begin_without_session_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("begin").await;
    assert!(core.status_message().contains("no active connection"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_begin_is_rejected() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("tx.db");
    seed(&db_path);
    let mut core = open(db_path).await;

    core.execute_command("begin").await;
    core.execute_command("begin").await;
    assert!(core.status_message().contains("already open"));
    core.execute_command("rollback").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_without_transaction_emits_status_only() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("tx.db");
    seed(&db_path);
    let mut core = open(db_path).await;

    core.execute_command("rollback").await;
    assert!(core.status_message().contains("no open transaction"));
}
