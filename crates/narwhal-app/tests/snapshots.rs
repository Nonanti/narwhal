//! Frame-buffer snapshot tests.
//!
//! Each test renders the headless [`AppCore`] into a deterministic
//! [`ratatui::backend::TestBackend`] and compares the resulting cell grid
//! against a stored snapshot via the `insta` crate. Run
//! `cargo insta review` after intentional UI changes to accept new
//! snapshots.

use std::path::PathBuf;

use insta::assert_snapshot;
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use tempfile::TempDir;
use uuid::Uuid;

const COLS: u16 = 120;
const ROWS: u16 = 32;

fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = buffer.cell((x, y)).expect("cell in bounds");
            out.push_str(cell.symbol());
        }
        // Trim trailing spaces so the snapshot stays compact and stable.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out
}

fn snapshot_core(core: &mut AppCore) -> String {
    snapshot_core_sized(core, COLS, ROWS)
}

fn snapshot_core_sized(core: &mut AppCore, cols: u16, rows: u16) -> String {
    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| core.render(frame, frame.area()))
        .expect("render");
    buffer_to_string(terminal.backend().buffer())
}

fn empty_state() -> AppCore {
    AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    )
}

fn configured_state(db_path: PathBuf) -> ConnectionsFile {
    ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "demo".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    }
}

#[test]
fn snapshot_empty_state() {
    let mut core = empty_state();
    assert_snapshot!("empty", snapshot_core(&mut core));
}

#[test]
fn snapshot_with_one_connection_configured() {
    let registry = DriverRegistry::with_defaults();
    let connections = configured_state(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    assert_snapshot!("connection_configured", snapshot_core(&mut core));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_after_query_returns_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('alpha'), ('beta'), ('gamma');",
        )
        .unwrap();
    }

    let registry = DriverRegistry::with_defaults();
    let connections = configured_state(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open demo").await;
    core.insert_into_editor("SELECT id, label FROM items ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;

    // Mask the elapsed-ms part of the status bar and result title so the
    // snapshot remains stable across runs.
    let mut rendered = snapshot_core(&mut core);
    rendered = mask_elapsed(rendered);
    assert_snapshot!("rows_after_run", rendered);
}

fn mask_elapsed(s: String) -> String {
    // " · N ms"  ->  " · ? ms"
    let re = regex_lite::Regex::new(r"\b\d+ ms\b").unwrap();
    re.replace_all(&s, "? ms").into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_help_modal() {
    let mut core = empty_state();
    core.open_help().await;
    assert!(core.help_open());
    assert_snapshot!("help_modal", snapshot_core_sized(&mut core, 130, 66));
}
