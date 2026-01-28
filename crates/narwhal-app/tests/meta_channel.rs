//! Integration tests for the background metadata channel (H11).
//!
//! Verifies that `dump_schema all`, `refresh_schemas`, and `open_history`
//! dispatch work to the meta channel and deliver results asynchronously
//! without blocking the UI.

use std::path::PathBuf;
use std::sync::Arc;

use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_history::{HistoryEntry, Journal};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "meta-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

/// H11: `dump_schema all` should not block the UI thread. The command
/// dispatches a `MetaRequest` and the result arrives asynchronously via
/// the meta channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dump_schema_all_does_not_block_ui() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("dump.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE one (id INTEGER PRIMARY KEY);
             CREATE TABLE two (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open meta-test");

    // Dispatch dump_schema all. It should return immediately (non-blocking).
    core.execute_command("dump-schema all");

    // The status should indicate the operation is in progress.
    assert!(
        core.status_message().contains("fetching DDL"),
        "expected in-progress status, got: {}",
        core.status_message()
    );

    // The editor should NOT yet contain the DDL (it's async).
    // Drain the meta channel to receive the result.
    core.drain_meta_updates().await;

    // After draining, the editor should contain the DDL.
    let editor = core.editor().entire_text();
    assert!(
        editor.contains("CREATE TABLE"),
        "editor should contain DDL after meta update, got: {editor:?}"
    );
    assert!(
        core.status_message().contains("wrote 2 table(s)"),
        "expected success status, got: {}",
        core.status_message()
    );
}

/// H11: `:refresh` dispatches a `MetaRequest::RefreshSchemas` and returns
/// immediately without blocking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_schemas_does_not_block_ui() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("refresh.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open meta-test");

    // Initial table count should be 1.
    let initial_count = core
        .session()
        .map_or(0, |s| s.schemas.iter().map(|(_, t)| t.len()).sum::<usize>());
    assert_eq!(initial_count, 1);

    // Dispatch refresh. It should return immediately.
    core.execute_command("refresh");
    assert!(
        core.status_message().contains("refreshing schema"),
        "expected in-progress status, got: {}",
        core.status_message()
    );

    // Drain the meta channel.
    core.drain_meta_updates().await;

    // After draining, the schema should be refreshed.
    assert!(
        core.status_message().contains("schema refreshed"),
        "expected refreshed status, got: {}",
        core.status_message()
    );
}

/// H11: `open_history` dispatches a `MetaRequest::LoadHistory` and returns
/// immediately without blocking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_history_does_not_block_ui() {
    let dir = TempDir::new().unwrap();
    let journal_path = dir.path().join("history.jsonl");
    let journal = Arc::new(Journal::open(&journal_path).await.unwrap());
    journal
        .append(&HistoryEntry::success("SELECT 1"))
        .await
        .unwrap();
    journal
        .append(&HistoryEntry::success("SELECT 2"))
        .await
        .unwrap();
    drop(journal);
    let journal = Arc::new(Journal::open(&journal_path).await.unwrap());

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "meta-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));

    // Open history. It should return immediately.
    core.open_history();
    assert!(
        core.status_message().contains("loading history"),
        "expected in-progress status, got: {}",
        core.status_message()
    );

    // Drain the meta channel.
    core.drain_meta_updates().await;

    // After draining, the history modal should be open.
    let state = core
        .history_state()
        .expect("modal should be open after meta update");
    assert_eq!(state.entries.len(), 2);
}
