//! integration tests for the `:pivot` command.

use std::path::PathBuf;

use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "pivot-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

async fn seed_result(core: &mut AppCore, db_path: PathBuf) {
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sales (country TEXT, year INTEGER, revenue INTEGER);
             INSERT INTO sales VALUES ('tr', 2024, 10);
             INSERT INTO sales VALUES ('tr', 2025, 20);
             INSERT INTO sales VALUES ('de', 2024, 15);
             INSERT INTO sales VALUES ('de', 2025, 30);",
        )
        .unwrap();
    }
    core.execute_command("open pivot-test").await;
    core.insert_into_editor("SELECT country, year, revenue FROM sales")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
}

fn render_once(core: &mut AppCore) {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("backend");
    terminal
        .draw(|frame| {
            core.render(frame, Rect::new(0, 0, 120, 30));
        })
        .expect("draw");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_on_sets_status_message() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("pivot rows=country").await;
    assert_eq!(core.status_message(), "pivot: on · agg=count");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_off_when_not_active_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("pivot off").await;
    assert_eq!(core.status_message(), "pivot: already hidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_on_then_off_clears_state() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("pivot rows=country agg=count").await;
    core.execute_command("pivot off").await;
    assert_eq!(core.status_message(), "pivot: off");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_pivot_active_does_not_panic_on_empty_result() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("pivot rows=country").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_sum_pivot_over_seeded_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("pivot rows=country cols=year value=revenue agg=sum")
        .await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_count_pivot_no_value() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("pivot rows=country").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_with_unknown_column_renders_placeholder() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("pivot rows=nope").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_sum_on_non_numeric_renders_placeholder() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("pivot rows=year value=country agg=sum")
        .await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pivot_survives_render_in_tiny_area() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("pivot rows=country").await;
    let backend = TestBackend::new(60, 8);
    let mut terminal = Terminal::new(backend).expect("backend");
    terminal
        .draw(|frame| {
            core.render(frame, Rect::new(0, 0, 60, 8));
        })
        .expect("draw");
}
