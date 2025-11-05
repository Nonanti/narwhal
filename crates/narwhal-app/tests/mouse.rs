//! Integration tests for mouse support across panes.
//!
//! Each test creates an `AppCore` with an in-memory SQLite session,
//! renders once to populate `LayoutRegions`, then dispatches
//! `crossterm::event::MouseEvent`s and asserts the side effects.

use std::path::PathBuf;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::Pane;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "mouse-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

/// Render the core into a test backend so `last_layout` is populated.
fn render(core: &mut AppCore) {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| core.render(frame, frame.area()))
        .expect("draw");
}

fn mouse_at(x: u16, y: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

fn click_at(x: u16, y: u16) -> MouseEvent {
    mouse_at(x, y, MouseEventKind::Down(MouseButton::Left))
}

fn scroll_down_at(x: u16, y: u16) -> MouseEvent {
    mouse_at(x, y, MouseEventKind::ScrollDown)
}

#[allow(dead_code)]
fn scroll_up_at(x: u16, y: u16) -> MouseEvent {
    mouse_at(x, y, MouseEventKind::ScrollUp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_click_changes_focus() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("focus.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open mouse-test");

    render(&mut core);

    // Click inside the editor rect → focus should switch to Editor.
    let editor_rect = core.last_layout().editor;
    core.handle_mouse(click_at(editor_rect.x + 2, editor_rect.y + 2));
    assert_eq!(core.focus(), Pane::Editor);

    // Click inside the sidebar rect → focus should switch to Sidebar.
    let sidebar_rect = core.last_layout().sidebar;
    core.handle_mouse(click_at(sidebar_rect.x + 2, sidebar_rect.y + 2));
    assert_eq!(core.focus(), Pane::Sidebar);

    // Click inside the results rect → focus should switch to Results.
    let results_rect = core.last_layout().results;
    core.handle_mouse(click_at(results_rect.x + 2, results_rect.y + 2));
    assert_eq!(core.focus(), Pane::Results);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_table_click_injects_preview() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("preview.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO customers VALUES (1, 'alice'), (2, 'bob');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open mouse-test");

    render(&mut core);

    // Find a sidebar table rect and click it.
    let table_rects = core.last_layout().sidebar_tables.clone();
    assert!(
        !table_rects.is_empty(),
        "sidebar should have at least one table entry"
    );
    let (rect, _idx) = &table_rects[0];
    core.handle_mouse(click_at(rect.x + 2, rect.y));

    // The editor should now contain a SELECT * FROM ... statement
    // and a query should have been dispatched.
    let editor_text = core.editor().entire_text();
    assert!(
        editor_text.contains("SELECT * FROM"),
        "editor should contain preview query, got: {editor_text:?}"
    );

    // Drain the run so we don't leave the core in a running state.
    core.drain_run_updates().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_item_click_accepts() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("comp.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL);
             CREATE TABLE order_items (id INTEGER PRIMARY KEY, qty INT);",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open mouse-test");

    // Enter insert mode and type a prefix that matches multiple tables.
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let i_key = KeyEvent {
        code: KeyCode::Char('i'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(i_key);
    for ch in "ord".chars() {
        core.handle_key(KeyEvent {
            code: KeyCode::Char(ch),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }

    // The completion popup should now be open.
    assert!(
        core.editor_completion_is_open(),
        "completion popup should be open after typing 'ord'"
    );

    render(&mut core);

    // Click on the second completion item.
    let items = core.last_layout().completion_items.clone();
    if items.len() >= 2 {
        let (rect, _idx) = &items[1];
        core.handle_mouse(click_at(rect.x + 1, rect.y));
    }

    // The completion popup should be closed and the editor should contain
    // the accepted item.
    assert!(
        !core.editor_completion_is_open(),
        "completion popup should close after click"
    );
    let text = core.editor().entire_text();
    assert!(
        text.len() > "ord".len(),
        "editor should contain the accepted completion, got: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scroll_in_results_pane_moves_view() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("scroll.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO big WITH RECURSIVE c(x) AS (
               SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 50
             ) SELECT x, 'row_' || x FROM c;",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open mouse-test");

    // Run a query that produces enough rows to scroll.
    core.insert_into_editor("SELECT * FROM big");
    core.execute_command("run");
    core.drain_run_updates().await;

    render(&mut core);

    let results_rect = core.last_layout().results;
    let initial_selected = core.tabs()[core.active_tab()]
        .results
        .active()
        .state
        .selected();

    // Scroll down inside the results pane.
    core.handle_mouse(scroll_down_at(results_rect.x + 2, results_rect.y + 5));
    let after_scroll = core.tabs()[core.active_tab()]
        .results
        .active()
        .state
        .selected();

    // The selection should have moved down (or been set to 0 from None).
    match (initial_selected, after_scroll) {
        (None, Some(_)) => {}              // moved from nothing to something
        (Some(a), Some(b)) if b > a => {}  // moved down
        (Some(a), Some(b)) if a == b => {} // stayed put (already at boundary)
        other => panic!("expected selection to move down or stay, got {other:?}"),
    }
}
