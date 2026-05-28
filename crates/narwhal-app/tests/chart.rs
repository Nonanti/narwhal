//! integration tests for the `:chart` command.
//!
//! The chart pane is a render-time projection of the active result;
//! these tests verify the command flow (`:chart bar` flips a flag on
//! the active tab, `:chart off` clears it), the status-bar feedback,
//! and that running a query while a chart is active does not panic
//! the render path even on degenerate (empty / NaN-adjacent) inputs.

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
            name: "chart-test".into(),
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
            "CREATE TABLE sales (country TEXT, revenue INTEGER);
             INSERT INTO sales VALUES ('tr', 10);
             INSERT INTO sales VALUES ('de', 30);
             INSERT INTO sales VALUES ('us', 20);
             INSERT INTO sales VALUES ('fr', 25);",
        )
        .unwrap();
    }
    core.execute_command("open chart-test").await;
    core.insert_into_editor("SELECT country, revenue FROM sales")
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
async fn chart_on_sets_status_message() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("chart bar").await;
    assert_eq!(core.status_message(), "chart: bar on");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chart_off_when_not_active_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("chart off").await;
    assert_eq!(core.status_message(), "chart: already hidden");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chart_on_then_off_clears_state() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("chart line").await;
    assert_eq!(core.status_message(), "chart: line on");
    core.execute_command("chart off").await;
    assert_eq!(core.status_message(), "chart: off");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_chart_active_does_not_panic_on_empty_result() {
    // No result yet, chart on — the chart placeholder must render
    // without dragging the whole frame down.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("chart bar").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_bar_chart_over_seeded_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("chart bar").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_line_chart_over_seeded_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("chart line --y revenue").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_sparkline_chart_over_seeded_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("chart sparkline --col revenue").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chart_with_unknown_column_renders_placeholder() {
    // Bad y-column override should hit the placeholder branch, not
    // crash the render path.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("chart bar --y nope").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chart_with_non_numeric_y_renders_placeholder() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    // `country` is text; selecting it as y should be rejected with a
    // placeholder, not a numeric coercion silently producing zeros.
    core.execute_command("chart bar --y country").await;
    render_once(&mut core);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chart_survives_render_in_tiny_area() {
    // Result pane height < 8 falls back to the no-split path; render
    // path should pick the full-table layout without complaining.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;
    core.execute_command("chart bar").await;
    // 30-cell wide × 8-cell tall: status bar + minimal panes — chart
    // pane gets clipped away.
    let backend = TestBackend::new(60, 8);
    let mut terminal = Terminal::new(backend).expect("backend");
    terminal
        .draw(|frame| {
            core.render(frame, Rect::new(0, 0, 60, 8));
        })
        .expect("draw");
}
