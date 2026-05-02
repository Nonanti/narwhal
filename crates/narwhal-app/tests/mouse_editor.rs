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

/// Regression: in basic/emacs mode, opening the `:` command prompt
/// then clicking on the editor body (or elsewhere) must cancel the
/// prompt. Without the fix, vim stays in Command mode and subsequent
/// characters are swallowed by the command buffer instead of being
/// inserted as SQL text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn click_cancels_command_prompt_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    render(&mut core);

    // Open the command prompt via `:`.
    let colon = KeyEvent {
        code: KeyCode::Char(':'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(colon).await;
    assert_eq!(
        core.mode(),
        narwhal_vim::Mode::Command,
        "`:` should put vim into Command mode"
    );

    // Click on the editor body — this must cancel command mode.
    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    assert_ne!(
        core.mode(),
        narwhal_vim::Mode::Command,
        "left click should cancel Command mode"
    );

    // Typing should now insert into the SQL buffer, not the command prompt.
    let a_key = KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(a_key).await;
    assert_eq!(
        core.editor().entire_text(),
        "a",
        "character should be inserted into the editor after click cancels command prompt"
    );
}

/// CB-9: clicking on a multi-byte UTF-8 line must not panic due to
/// landing on a continuation byte. The display-width walk in
/// `editor_click_to_buffer_pos` must find a valid char boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_click_does_not_panic_on_multibyte_line() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    core.insert_into_editor("merhaba dünya naïve").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    // Click at several offsets across the multi-byte text.
    // None of these should panic.
    for offset in 0..20u16 {
        core.handle_mouse(mouse(
            ox + offset,
            oy,
            MouseEventKind::Down(MouseButton::Left),
        ))
        .await;
    }
    // Double-click inside the word "dünya" (visual col ~9).
    core.handle_mouse(mouse(ox + 9, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    core.handle_mouse(mouse(ox + 9, oy, MouseEventKind::Down(MouseButton::Left)))
        .await;
    // No panic is the success criterion; also verify cursor is on a
    // valid line.
    assert!(core.editor().cursor_row() == 0);
}

/// MC2: when vim is in Command mode, keyboard-driven focus changes
/// (Ctrl-W) must not strand keystrokes — subsequent keys should
/// still route to the editor's command handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_mode_keyboard_focus_drift_protected() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    // Switch to vim mode for this test.
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Vim;
    settings.editor.mouse = MouseSelectionMode::Enabled;
    core.apply_settings(settings);
    render(&mut core);

    // Enter command mode via `:`.
    let colon = KeyEvent {
        code: KeyCode::Char(':'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(colon).await;
    assert_eq!(core.mode(), narwhal_vim::Mode::Command);

    // Cycle focus via Ctrl-W — this would move focus to Sidebar.
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(ctrl_w).await;

    // Even though focus might have moved, typing 's' in Command mode
    // should still be handled by the editor (command buffer), not
    // by the sidebar handler.
    let s_key = KeyEvent {
        code: KeyCode::Char('s'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(s_key).await;

    // The vim layer should still be in Command mode (the 's' went
    // into the command buffer) OR it exited cleanly — either way
    // the sidebar must not have processed the 's' as a sidebar action.
    // In vim Command mode, 's' is just a character in the prompt.
    // The key test: we must not have crashed and the sidebar index
    // should be unchanged from its initial position.
    let sidebar_idx = core.ui_for_test().sidebar_index;
    assert_eq!(sidebar_idx, 0, "sidebar should not have reacted to 's'");
}

/// CB-1: when a keyboard-owning modal (e.g. :settings) is open,
/// mouse clicks must not mutate background pane state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modal_keyboard_owner_blocks_mouse_state_mutation() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    render(&mut core);

    // Open the settings modal.
    core.execute_command("settings").await;
    assert!(
        core.settings_modal_open(),
        ":settings should open the modal"
    );

    let sidebar_idx_before = core.ui_for_test().sidebar_index;
    let focus_before = core.focused_pane();

    // Click on the sidebar area — this should be blocked.
    let sidebar_rect = core.last_layout().sidebar;
    core.handle_mouse(mouse(
        sidebar_rect.x + 2,
        sidebar_rect.y + 2,
        MouseEventKind::Down(MouseButton::Left),
    ))
    .await;

    // Modal should still be open.
    assert!(
        core.settings_modal_open(),
        "settings modal should remain open after background click"
    );
    // Sidebar state should not have changed.
    assert_eq!(
        core.ui_for_test().sidebar_index,
        sidebar_idx_before,
        "sidebar index must not change while modal owns keyboard"
    );
    // Focus should not have changed.
    assert_eq!(
        core.focused_pane(),
        focus_before,
        "focus must not change while modal owns keyboard"
    );
}

/// CB-7: `mouse_mode=disabled` blocks right-click context menu.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_disabled_swallows_right_click_menu() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Disabled;
    core.apply_settings(settings);
    core.insert_into_editor("select 1").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox + 2, oy, MouseEventKind::Down(MouseButton::Right)))
        .await;
    assert!(
        !core.context_menu_open(),
        "right-click must not open the context menu when mouse is disabled"
    );
}

/// CB-7: `mouse_mode=disabled` blocks middle-click paste.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_disabled_swallows_middle_click_paste() {
    let clipboard = Arc::new(InMemoryClipboard::new());
    clipboard.set_text("PASTE").unwrap();
    let mut core = setup(clipboard).await;
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Disabled;
    core.apply_settings(settings);
    core.insert_into_editor("ab").await;
    render(&mut core);

    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox + 1, oy, MouseEventKind::Down(MouseButton::Middle)))
        .await;
    assert!(
        !core.editor().entire_text().contains("PASTE"),
        "middle-click must not paste when mouse is disabled, got {:?}",
        core.editor().entire_text(),
    );
}

/// CB-7: `mouse_mode=disabled` blocks scroll events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_disabled_swallows_scroll() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Disabled;
    core.apply_settings(settings);
    core.insert_into_editor("line1\nline2\nline3\nline4\nline5")
        .await;
    render(&mut core);

    let cursor_before = core.editor().cursor_row();
    let (ox, oy) = editor_text_origin(&core);
    core.handle_mouse(mouse(ox, oy, MouseEventKind::ScrollDown))
        .await;
    core.handle_mouse(mouse(ox, oy, MouseEventKind::ScrollDown))
        .await;
    assert_eq!(
        core.editor().cursor_row(),
        cursor_before,
        "scroll must not move cursor when mouse is disabled"
    );
}

/// CB-7: `mouse_mode=disabled` blocks pane focus change via left-click.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_disabled_swallows_pane_focus_change() {
    let mut core = setup(Arc::new(InMemoryClipboard::new())).await;
    let mut settings = Settings::default();
    settings.editor.mode = EditorMode::Basic;
    settings.editor.mouse = MouseSelectionMode::Disabled;
    core.apply_settings(settings);
    render(&mut core);

    let focus_before = core.focused_pane();
    // Click on the sidebar area — should be blocked.
    let sidebar_rect = core.last_layout().sidebar;
    core.handle_mouse(mouse(
        sidebar_rect.x + 2,
        sidebar_rect.y + 2,
        MouseEventKind::Down(MouseButton::Left),
    ))
    .await;
    assert_eq!(
        core.focused_pane(),
        focus_before,
        "pane focus must not change when mouse is disabled"
    );
}
