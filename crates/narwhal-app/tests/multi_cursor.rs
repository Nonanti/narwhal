//! integration tests for multi-cursor editing in the editor pane.
//!
//! MVP scope: Alt-N (add next), Alt-A (add all), Esc (collapse), and
//! character-level edit propagation. Vim block-visual interop, undo /
//! redo across cursors, and column-mode (Ctrl-Alt-arrows) are deferred
//! to v2.1.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
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
            name: "mc-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

const fn alt(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::ALT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn make_core() -> AppCore {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path);
    let core = AppCore::new(registry, connections, None);
    // Keep the TempDir alive by leaking it; tests are short-lived so
    // the OS reclaims when the process exits.
    std::mem::forget(dir);
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_n_adds_secondary_cursor_at_next_match() {
    let mut core = make_core();
    core.insert_into_editor("foo bar foo baz").await;
    // Move primary into the first `foo` (col 2).
    {
        let idx = core.active_tab();
        &mut core.tabs_mut()[idx]
    }
    .editor_mut()
    .set_cursor(0, 2);
    core.handle_key(alt('n')).await;
    let secondaries = core.tabs()[core.active_tab()]
        .editor()
        .secondary_cursors()
        .to_vec();
    assert_eq!(secondaries, vec![(0, 11)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_a_adds_all_secondaries() {
    let mut core = make_core();
    core.insert_into_editor("id id id").await;
    {
        let idx = core.active_tab();
        &mut core.tabs_mut()[idx]
    }
    .editor_mut()
    .set_cursor(0, 0);
    core.handle_key(alt('a')).await;
    let secondaries = core.tabs()[core.active_tab()]
        .editor()
        .secondary_cursors()
        .to_vec();
    // `id` matches at col 0 (contains primary, skipped), 3, 6 → ends 5, 8
    assert_eq!(secondaries, vec![(0, 5), (0, 8)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_collapses_multi_cursor() {
    let mut core = make_core();
    core.insert_into_editor("foo foo").await;
    {
        let idx = core.active_tab();
        &mut core.tabs_mut()[idx]
    }
    .editor_mut()
    .set_cursor(0, 0);
    core.handle_key(alt('n')).await;
    assert!(core.tabs()[core.active_tab()].editor().has_multi_cursors());
    core.handle_key(key(KeyCode::Esc)).await;
    assert!(!core.tabs()[core.active_tab()].editor().has_multi_cursors());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn render_with_multi_cursor_does_not_panic() {
    let mut core = make_core();
    core.insert_into_editor("foo foo foo").await;
    {
        let idx = core.active_tab();
        &mut core.tabs_mut()[idx]
    }
    .editor_mut()
    .set_cursor(0, 0);
    core.handle_key(alt('a')).await;
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("backend");
    terminal
        .draw(|frame| {
            core.render(frame, Rect::new(0, 0, 80, 20));
        })
        .expect("draw");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_n_without_word_reports_no_match() {
    let mut core = make_core();
    core.insert_into_editor("   ").await;
    {
        let idx = core.active_tab();
        &mut core.tabs_mut()[idx]
    }
    .editor_mut()
    .set_cursor(0, 1);
    core.handle_key(alt('n')).await;
    assert_eq!(
        core.status_message(),
        "multi-cursor: no match for word under cursor"
    );
}
