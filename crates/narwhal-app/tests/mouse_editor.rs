//! Mouse interaction tests for the editor pane.
//!
//! Covers single-click cursor positioning, drag selection,
//! double-click word selection, triple-click line selection,
//! middle-click paste, and right-click context menu.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use narwhal_app::DriverRegistry;
use narwhal_app::clipboard::{Clipboard, InMemoryClipboard};
use narwhal_app::core::AppCore;
use narwhal_config::{ConnectionsFile, EditorMode, InMemoryStore, MouseSelectionMode, Settings};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "mouse-edit".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

fn render(core: &mut AppCore) {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| core.render(frame, frame.area()))
        .expect("draw");
}

const fn mouse(x: u16, y: u16, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

async fn setup(clipboard: Arc<dyn Clipboard>) -> AppCore {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("mouse-edit.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::with_services(
        registry,
        connections,
        None,
        Arc::new(InMemoryStore::new()),
        clipboard,
    );
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Enabled;
    core.apply_settings(settings);
    core.execute_command("open mouse-edit").await;
    // Keep the dir alive by leaking the handle for the test
    // lifetime — TempDir would otherwise drop on scope exit.
    std::mem::forget(dir);
    core
}

/// Approximate buffer→screen mapping: the editor occupies the right
/// side after the sidebar (default width 24 cells) and starts at
/// row 1 (above-status border). The exact column inside the inner
/// area is `sidebar_w + 1 (border) + gutter_w` where `gutter_w` is 6
/// for buffers up to 999 lines. For a fresh buffer the gutter is 6;
/// we pre-render to fix `last_layout` and then read the editor rect
/// directly.
fn editor_text_origin(core: &AppCore) -> (u16, u16) {
    let layout = core.last_layout();
    let inner_x = layout.editor.x + 1; // border
    let inner_y = layout.editor.y + 1;
    let gutter = narwhal_tui::gutter_width(core.editor().line_count()) as u16;
    (inner_x + gutter, inner_y)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_click_positions_cursor() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("hello world").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    // Click at column 6 ("hello |world" — between 'o' and ' ').
    core.handle_mouse(mouse(ox + 6, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    let buf = core.editor();
    assert_eq!(buf.cursor_row(), 0);
    assert_eq!(buf.cursor_col(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drag_extends_selection() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("hello world").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    core.handle_mouse(mouse(ox + 5, oy, MouseEventKind::Drag(MouseButton::Left)))
        .await;
    core.handle_mouse(mouse(ox + 5, oy, MouseEventKind::Up(MouseButton::Left)))
        .await;
    assert_eq!(core.editor().selected_text(), "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_click_selects_word() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("hello world").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    // First click positions cursor inside "hello".
    core.handle_mouse(mouse(ox + 2, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    // Second click within the multi-click window selects the word.
    core.handle_mouse(mouse(ox + 2, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    assert_eq!(core.editor().selected_text(), "hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn triple_click_selects_line() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("one\ntwo three").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    let target_y = oy + 1; // second line
    core.handle_mouse(mouse(
        ox + 4,
        target_y,
        MouseEventKind::Down(MouseButton::Left),
    ))
    .await;
    core.handle_mouse(mouse(
        ox + 4,
        target_y,
        MouseEventKind::Down(MouseButton::Left),
    ))
    .await;
    core.handle_mouse(mouse(
        ox + 4,
        target_y,
        MouseEventKind::Down(MouseButton::Left),
    ))
    .await;
    // The whole second line is selected.
    assert!(
        core.editor().selected_text().contains("two three"),
        "selection should include the full line, got {:?}",
        core.editor().selected_text(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn middle_click_pastes_at_cursor() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    clipboard.set_text("INSERT").unwrap();
    let mut core = setup(clipboard).await;
    core.insert_into_editor("ab").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox + 1, oy, MouseEventKind::Down(MouseButton::Middle)))
        .await;
    assert!(
        core.editor().entire_text().contains("INSERT"),
        "buffer should include pasted text, got {:?}",
        core.editor().entire_text(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn right_click_opens_context_menu() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("select 1").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox + 2, oy, MouseEventKind::Down(MouseButton::Right)))
        .await;
    assert!(core.context_menu_open(), "right-click should open the menu");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_disabled_swallows_drag() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Disabled;
    core.apply_settings(settings);
    core.insert_into_editor("hello world").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    core.handle_mouse(mouse(ox + 5, oy, MouseEventKind::Drag(MouseButton::Left)))
        .await;
    assert_eq!(core.editor().selected_text(), "");
}
