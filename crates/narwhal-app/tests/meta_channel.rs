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

/// C5 regression: `dump_schema all` addresses the originating tab by
/// its stable id. If the user closes that tab between dispatch and
/// reply, the DDL must NOT be written into an unrelated tab; instead
/// the update is dropped with a status message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dump_schema_drops_reply_when_originating_tab_closed() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("close.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE alpha (id INTEGER PRIMARY KEY);
             CREATE TABLE beta  (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open meta-test");

    // Open a second tab and dispatch dump-schema *from there*. The
    // originating tab is tab 1 (id=2); tab 0 carries id=1.
    core.execute_command("tabnew");
    assert_eq!(core.tabs().len(), 2);
    assert_eq!(core.active_tab(), 1);
    let originating_tab_id = core.tabs()[1].id();
    core.execute_command("dump-schema all");

    // Close the originating tab BEFORE the reply arrives. `tabclose`
    // closes whichever tab is active, so we close tab 1 directly
    // (no need to switch first). With index-based addressing this
    // would (a) panic on `self.tabs[1]` access, or (b) write into
    // the only remaining tab — both wrong. The stable-id resolution
    // should drop the reply and surface a status message.
    core.execute_command("tabclose");

    // Sanity: tab 0 (the original initial tab) is now alone and its
    // id is NOT the originating id.
    assert_eq!(core.tabs().len(), 1);
    assert_ne!(core.tabs()[0].id(), originating_tab_id);
    let editor_before = core.editor().entire_text();

    core.drain_meta_updates().await;

    let editor_after = core.editor().entire_text();
    assert_eq!(
        editor_before, editor_after,
        "the surviving tab's editor must NOT be overwritten with DDL meant for a closed tab"
    );
    assert!(
        core.status_message().contains("target tab was closed"),
        "expected drop-on-close status, got: {}",
        core.status_message()
    );
}

/// H8 regression: `SchemasRefreshed` carries the originating session
/// id; replies that arrive after the user switched (or closed) the
/// session must NOT overwrite the new session's listing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_schemas_drops_reply_when_session_changed() {
    let dir = TempDir::new().unwrap();
    let db_a = dir.path().join("a.db");
    let db_b = dir.path().join("b.db");
    {
        let conn = rusqlite::Connection::open(&db_a).unwrap();
        conn.execute_batch("CREATE TABLE a_only (id INTEGER PRIMARY KEY)")
            .unwrap();
        let conn = rusqlite::Connection::open(&db_b).unwrap();
        conn.execute_batch(
            "CREATE TABLE b_one (id INTEGER PRIMARY KEY);
             CREATE TABLE b_two (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    }

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![
            ConnectionConfig {
                id: Uuid::new_v4(),
                name: "conn-a".into(),
                driver: "sqlite".into(),
                params: ConnectionParams {
                    path: Some(db_a.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            },
            ConnectionConfig {
                id: Uuid::new_v4(),
                name: "conn-b".into(),
                driver: "sqlite".into(),
                params: ConnectionParams {
                    path: Some(db_b.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            },
        ],
    };
    let mut core = AppCore::new(registry, connections, None);

    // Open A, dispatch refresh (stale reply will target A).
    core.execute_command("open conn-a");
    core.execute_command("refresh");

    // Before draining the stale reply, switch to B and let B's
    // schemas be loaded directly via `open` (B has 2 tables).
    core.execute_command("open conn-b");
    let b_table_count_before = core
        .session()
        .map_or(0, |s| s.schemas.iter().map(|(_, t)| t.len()).sum::<usize>());
    assert_eq!(b_table_count_before, 2);

    // Now drain. The stale A-refresh reply arrives carrying A's
    // session id; the handler must drop it instead of overwriting
    // B's schema listing.
    core.drain_meta_updates().await;

    let b_table_count_after = core
        .session()
        .map_or(0, |s| s.schemas.iter().map(|(_, t)| t.len()).sum::<usize>());
    assert_eq!(
        b_table_count_after, 2,
        "B's schemas must be intact after a stale A-refresh reply"
    );
}
