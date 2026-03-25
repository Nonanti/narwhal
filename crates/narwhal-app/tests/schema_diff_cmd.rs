//! Integration tests for the `:schema-diff` TUI command.
//!
//! Runs against two in-process `SQLite` databases so the test does
//! not need Docker. The command must:
//!
//! - Resolve `source` / `target` connection names against the
//!   in-memory connections file.
//! - Refuse a self-diff (same id on both sides).
//! - Drop the emitted DDL into a fresh editor tab on a non-empty
//!   diff and surface a "no changes" status on an empty one.
//! - Honour `--dialect`, `--schema`, `--table`, `--schema-map`.

use std::path::PathBuf;

use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;

fn active_tab_text(core: &AppCore) -> String {
    core.editor().entire_text()
}
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

fn make_db(path: &std::path::Path, ddl: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(ddl).unwrap();
}

fn fixture(src_path: PathBuf, tgt_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![
            ConnectionConfig {
                id: Uuid::from_u128(1),
                name: "src".into(),
                driver: "sqlite".into(),
                params: ConnectionParams::with(|p| {
                    p.path = Some(src_path.to_string_lossy().into_owned());
                }),
            },
            ConnectionConfig {
                id: Uuid::from_u128(2),
                name: "tgt".into(),
                driver: "sqlite".into(),
                params: ConnectionParams::with(|p| {
                    p.path = Some(tgt_path.to_string_lossy().into_owned());
                }),
            },
        ],
    };
    (registry, connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drift_lands_in_new_tab() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(
        &src,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL);",
    );
    make_db(&tgt, "CREATE TABLE users (id INTEGER PRIMARY KEY);");

    let (registry, connections) = fixture(src, tgt);
    let starting_tab_count = {
        let mut core = AppCore::new(registry, connections, None);
        // Sanity-check the starting condition.
        let initial = core.tabs().len();
        core.execute_command("schema-diff src tgt --dialect sqlite")
            .await;
        let after = core.tabs().len();
        assert!(
            after > initial,
            "a new tab must be opened: {initial} -> {after}"
        );
        let buf = active_tab_text(&core);
        assert!(
            buf.contains("-- schema-diff: src  ->  tgt"),
            "buffer header missing: {buf}"
        );
        // The new column `email` must show up as an ADD COLUMN
        // statement (SQLite supports that natively).
        assert!(
            buf.contains("ADD COLUMN email"),
            "expected ADD COLUMN, got:\n{buf}"
        );
        assert!(
            core.status_message().contains("table change(s)"),
            "status missing change summary: {}",
            core.status_message(),
        );
        initial
    };
    // Tab count is implementation-defined (1 vs 2 depending on
    // whether the default tab was reused) but must have grown.
    let _ = starting_tab_count;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_schemas_skip_tab_open() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(
        &src,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);",
    );
    make_db(
        &tgt,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);",
    );

    let (registry, connections) = fixture(src, tgt);
    let mut core = AppCore::new(registry, connections, None);
    let before = core.tabs().len();
    core.execute_command("schema-diff src tgt --dialect sqlite")
        .await;
    let after = core.tabs().len();
    assert_eq!(
        before, after,
        "no new tab on empty diff: {before} -> {after}"
    );
    assert!(
        core.status_message().contains("identical"),
        "status must say identical; got: {}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_source_surfaces_status() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(&src, "CREATE TABLE t (id INTEGER);");
    make_db(&tgt, "CREATE TABLE t (id INTEGER);");
    let (registry, connections) = fixture(src, tgt);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("schema-diff ghost tgt").await;
    assert!(
        core.status_message().contains("source `ghost` not found"),
        "got: {}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_diff_is_refused() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(&src, "CREATE TABLE t (id INTEGER);");
    make_db(&tgt, "CREATE TABLE t (id INTEGER);");
    let (registry, connections) = fixture(src, tgt);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("schema-diff src src").await;
    assert!(
        core.status_message()
            .contains("source and target both refer to"),
        "got: {}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_dialect_is_rejected_before_introspection() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(&src, "CREATE TABLE t (id INTEGER);");
    make_db(&tgt, "CREATE TABLE t (id INTEGER, extra TEXT);");
    let (registry, connections) = fixture(src, tgt);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("schema-diff src tgt --dialect bogus")
        .await;
    assert!(
        core.status_message().contains("unknown dialect `bogus`"),
        "got: {}",
        core.status_message(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn table_filter_narrows_scope() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src.db");
    let tgt = dir.path().join("tgt.db");
    make_db(
        &src,
        "CREATE TABLE users (id INTEGER, email TEXT); \
         CREATE TABLE orders (id INTEGER, total REAL);",
    );
    make_db(
        &tgt,
        "CREATE TABLE users (id INTEGER); \
         CREATE TABLE orders (id INTEGER);",
    );

    let (registry, connections) = fixture(src, tgt);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("schema-diff src tgt --dialect sqlite --table users")
        .await;
    let buf = active_tab_text(&core);
    // `email` column on `users` must appear; `total` on `orders`
    // must NOT — it was filtered out.
    assert!(buf.contains("ADD COLUMN email"), "got:\n{buf}");
    // Don't bare-match `total` (header says "N total delta(s)");
    // grep for the actual ADD COLUMN that would leak from `orders`.
    assert!(
        !buf.contains("ADD COLUMN total"),
        "filtered table leaked:\n{buf}"
    );
    assert!(
        !buf.contains("orders"),
        "orders surface name leaked:\n{buf}"
    );
}
