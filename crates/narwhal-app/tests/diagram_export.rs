//! End-to-end tests for `:diagram export`.
//!
//! Creates an `AppCore` backed by an on-disk `SQLite` database with a
//! small schema, opens the connection, then drives the command palette
//! to verify Mermaid / DOT export and `--table` focus.

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
            name: "diagram-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

fn seed(db_path: &PathBuf) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE users (
             id    INTEGER PRIMARY KEY,
             email TEXT NOT NULL UNIQUE
         );
         CREATE TABLE orders (
             id      INTEGER PRIMARY KEY,
             user_id INTEGER NOT NULL,
             status  TEXT,
             FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
         );
         CREATE TABLE order_items (
             order_id   INTEGER NOT NULL,
             product_id INTEGER NOT NULL,
             qty        INTEGER NOT NULL,
             PRIMARY KEY (order_id, product_id),
             FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE
         );
         CREATE TABLE audit (
             id       INTEGER PRIMARY KEY,
             actor_id INTEGER,
             FOREIGN KEY (actor_id) REFERENCES users(id)
         );",
    )
    .unwrap();
}

async fn open_seeded_session() -> (TempDir, AppCore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("schema.db");
    seed(&db_path);
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open diagram-test").await;
    core.drain_run_updates_and_refresh().await;
    (dir, core)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mermaid_export_to_file_writes_er_diagram() {
    let (dir, mut core) = open_seeded_session().await;
    let out_path = dir.path().join("schema.mmd");

    core.execute_command(&format!("diagram export mermaid {}", out_path.display()))
        .await;

    let body = std::fs::read_to_string(&out_path).expect("mmd file");
    assert!(body.starts_with("---\ntitle: narwhal schema\n---\nerDiagram\n"));
    // SQLite reports the default schema as `main`, so qualified ids end
    // up as `main_<table>`. (On Postgres this would be `public_...`.)
    assert!(body.contains("main_users {"), "users node missing:\n{body}");
    assert!(body.contains("main_orders {"));
    assert!(body.contains("main_order_items {"));
    // FK edges with parent (users) on the LEFT and child (orders) on RIGHT.
    assert!(
        body.contains("main_users ||--o{ main_orders"),
        "missing users→orders edge:\n{body}"
    );
    assert!(body.contains("main_orders ||--o{ main_order_items"));
    // Nullable FK (audit.actor_id) → `|o--o{`.
    assert!(
        body.contains("main_users |o--o{ main_audit"),
        "expected nullable FK marker:\n{body}"
    );
    // Status surfaced format + counts.
    let status = core.status_message();
    assert!(status.contains("mermaid"), "status: {status}");
    assert!(status.contains("4 tables"), "status: {status}");
    assert!(status.contains("3 edges"), "status: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dot_export_default_extension_added() {
    let (dir, mut core) = open_seeded_session().await;
    // No extension on the path — handler must append `.dot`.
    let base = dir.path().join("schema");

    core.execute_command(&format!("diagram export dot {}", base.display()))
        .await;

    let with_ext = dir.path().join("schema.dot");
    let body = std::fs::read_to_string(&with_ext).expect("dot file");
    assert!(body.starts_with("digraph schema {\n"));
    assert!(
        body.contains("main_orders:user_id -> main_users:id"),
        "missing FK edge in DOT:\n{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn focused_table_filters_to_one_hop_neighbours() {
    let (dir, mut core) = open_seeded_session().await;
    let out_path = dir.path().join("focus.mmd");

    core.execute_command(&format!(
        "diagram export mermaid {} --table orders",
        out_path.display()
    ))
    .await;

    let body = std::fs::read_to_string(&out_path).expect("focus file");
    assert!(body.contains("main_orders {"));
    assert!(body.contains("main_users {"), "users is a 1-hop out-edge");
    assert!(
        body.contains("main_order_items {"),
        "order_items is a 1-hop in-edge"
    );
    // 2 hops away (audit → users → orders) → must not appear.
    assert!(!body.contains("main_audit {"), "audit is 2 hops away:\n{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clipboard_path_used_when_no_file_given() {
    let (_dir, mut core) = open_seeded_session().await;

    core.execute_command("diagram export mermaid").await;
    let status = core.status_message();
    assert!(
        status.contains("clipboard"),
        "expected clipboard status, got: {status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_table_reports_friendly_error() {
    let (_dir, mut core) = open_seeded_session().await;

    core.execute_command("diagram export mermaid --table no_such_table")
        .await;
    let status = core.status_message();
    assert!(status.contains("not found"), "status: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_active_connection_reports_friendly_error() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("schema.db");
    seed(&db_path);
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    // No `:open` here — there is no active session.
    core.execute_command("diagram export mermaid").await;
    let status = core.status_message();
    assert!(status.contains("no active connection"), "status: {status}");
}
