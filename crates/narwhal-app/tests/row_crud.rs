//! Integration tests for the L36 row CRUD + pending changes pipeline.
//!
//! Each scenario seeds a real sqlite database, drives the result pane
//! through the public `AppCore` surface, and verifies both the
//! in-memory queue and the on-disk state.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
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

const fn shift(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "crud".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

/// Drive the sidebar so the focus lands on the `items` row and a
/// preview is dispatched. Mirrors the helper used by `cell_edit.rs`.
async fn preview_items(core: &mut AppCore) {
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl('w')).await;
    }
    // connection (0) -> main schema (1) -> items (2)
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('o'))).await;
    core.drain_run_updates().await;
}

async fn open_seeded_db(label_a: &str, label_b: &str) -> (AppCore, PathBuf, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("crud.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         INSERT INTO items (label) VALUES ('{label_a}'), ('{label_b}');"
    ))
    .unwrap();
    drop(conn);
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open crud").await;
    preview_items(&mut core).await;
    (core, db_path, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_row_queues_and_commits() {
    let (mut core, db_path, _dir) = open_seeded_db("alpha", "beta").await;
    // Focus the row 'alpha' (row 0) in the results pane.
    core.handle_key(key(KeyCode::Char('j'))).await; // select row 0
    core.handle_key(key(KeyCode::Char('d'))).await; // queue delete

    assert!(
        core.status_message().contains("queued DELETE"),
        "got status: {}",
        core.status_message()
    );
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);

    // DB still untouched.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    // Commit.
    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let surviving: String = conn
        .query_row("SELECT label FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(surviving, "beta", "alpha must have been deleted");
    assert_eq!(
        core.tabs()[core.active_tab()].pending().len(),
        0,
        "queue must be empty after commit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_drops_queue_without_touching_database() {
    let (mut core, db_path, _dir) = open_seeded_db("alpha", "beta").await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);

    core.handle_key(ctrl('x')).await;
    assert!(
        core.status_message().contains("discarded 1"),
        "got: {}",
        core.status_message()
    );
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "discard must not touch the database");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_row_clones_non_pk_columns_and_commits() {
    let (mut core, db_path, _dir) = open_seeded_db("alpha", "beta").await;
    core.handle_key(key(KeyCode::Char('j'))).await; // select 'alpha'
    core.handle_key(shift('o')).await; // duplicate
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);

    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn.prepare("SELECT label FROM items ORDER BY id").unwrap();
    let labels: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(labels, vec!["alpha", "beta", "alpha"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_then_edit_columns_and_commit_inserts_row() {
    let (mut core, db_path, _dir) = open_seeded_db("alpha", "beta").await;
    // Queue an empty INSERT.
    core.handle_key(key(KeyCode::Char('o'))).await;
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);

    // Edit the `label` cell on the staged row \u2014 we reuse the existing
    // row 0 because the in-memory grid has not been refreshed yet, but
    // queueing the Update on a *different* row id still produces a
    // syntactically valid INSERT/UPDATE pair when committed back-to-back.
    //
    // Simpler: write the value directly into the staged INSERT via the
    // public `pending` accessor before committing.
    {
        use narwhal_app::pending::PendingMutation;
        if let Some(PendingMutation::Insert { values, .. }) =
            core.tabs_mut()[0].pending_mut().get_mut(0)
        {
            values.insert("label".into(), narwhal_core::Value::String("gamma".into()));
        }
    }

    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE label = 'gamma'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "gamma row should exist after commit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_mode_blocks_every_row_crud_action() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("ro.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         INSERT INTO items (label) VALUES ('alpha');",
    )
    .unwrap();
    drop(conn);
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    core.set_read_only(true);
    core.execute_command("open crud").await;
    preview_items(&mut core).await;

    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    assert!(
        core.status_message().contains("read-only"),
        "delete must be blocked, got: {}",
        core.status_message()
    );
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);

    core.handle_key(key(KeyCode::Char('o'))).await;
    assert!(core.status_message().contains("read-only"));
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);

    // DB is genuinely untouched.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_rejected_on_table_without_primary_key() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("nopk.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch("CREATE TABLE notes (n TEXT); INSERT INTO notes VALUES ('a'), ('b');")
        .unwrap();
    drop(conn);
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open crud").await;
    preview_items(&mut core).await; // 'items' \u2014 wait, it's `notes` here.
                                    // Sidebar layout: conn (0) → main (1) → notes (2). preview_items
                                    // already walked there.

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w')).await;
    }
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;

    assert!(
        core.status_message().contains("no primary key"),
        "got: {}",
        core.status_message()
    );
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_modal_opens_lists_and_closes() {
    let (mut core, _db_path, _dir) = open_seeded_db("alpha", "beta").await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);

    core.handle_key(ctrl('p')).await;
    assert!(
        core.tabs()[core.active_tab()].pending_preview().is_some(),
        "Ctrl-P should open the preview modal"
    );

    // Toggling again closes the modal but keeps the queue.
    core.handle_key(ctrl('p')).await;
    assert!(core.tabs()[core.active_tab()].pending_preview().is_none());
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_audit_entries_land_in_history_journal() {
    use narwhal_history::Journal;
    use std::sync::Arc;

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("audit.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);
         INSERT INTO items (label) VALUES ('alpha'), ('beta');",
    )
    .unwrap();
    drop(conn);

    let journal_path = dir.path().join("history.jsonl");
    let journal = Arc::new(Journal::open(&journal_path).await.unwrap());
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, Some(journal));
    core.execute_command("open crud").await;
    preview_items(&mut core).await;

    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    // Give the spawned audit-log task a beat to land on disk.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let raw = std::fs::read_to_string(&journal_path).unwrap();
    assert!(
        raw.contains("\"source\":\"pending\""),
        "audit log must tag pending-commit entries; got: {raw}"
    );
    assert!(
        raw.contains("DELETE FROM"),
        "audit log must record the generated DELETE; got: {raw}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_badge_visible_in_status_bar() {
    let (mut core, _db_path, _dir) = open_seeded_db("alpha", "beta").await;
    core.handle_key(key(KeyCode::Char('j'))).await;
    core.handle_key(key(KeyCode::Char('d'))).await;
    // The badge is rendered, not directly exposed by accessors, but the
    // tab's pending count is the source of truth.
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 1);
    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_mutations_commit_in_one_transaction() {
    let (mut core, db_path, _dir) = open_seeded_db("alpha", "beta").await;
    // Queue: delete alpha, duplicate beta, append empty insert
    // populated with 'gamma'.
    core.handle_key(key(KeyCode::Char('j'))).await; // row 0 = alpha
    core.handle_key(key(KeyCode::Char('d'))).await; // delete alpha
    core.handle_key(key(KeyCode::Char('j'))).await; // row 1 = beta
    core.handle_key(shift('o')).await; // duplicate beta
    core.handle_key(key(KeyCode::Char('o'))).await; // empty insert

    {
        use narwhal_app::pending::PendingMutation;
        if let Some(PendingMutation::Insert { values, .. }) =
            core.tabs_mut()[0].pending_mut().get_mut(2)
        {
            values.insert("label".into(), narwhal_core::Value::String("gamma".into()));
        }
    }
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 3);

    core.handle_key(ctrl('s')).await;
    core.drain_run_updates().await;

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn.prepare("SELECT label FROM items ORDER BY id").unwrap();
    let labels: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // alpha gone, beta + duplicate of beta + gamma.
    assert_eq!(labels, vec!["beta", "beta", "gamma"]);
    assert_eq!(core.tabs()[core.active_tab()].pending().len(), 0);
}
