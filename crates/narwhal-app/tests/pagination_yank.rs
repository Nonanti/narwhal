//! Integration tests for pagination (`:next` / `:prev` / `:page-size`)
//! and clipboard yank (`y` / `Y`).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::clipboard::{Clipboard, InMemoryClipboard};
use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::Pane;
use tempfile::TempDir;
use uuid::Uuid;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Build a fresh AppCore wired to an in-memory clipboard and credential
/// store, pointed at a freshly-seeded sqlite database with `items` (1..=count).
async fn seeded(count: usize) -> (AppCore, Arc<InMemoryClipboard>, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("p.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)",
            [],
        )
        .unwrap();
        let mut stmt = conn
            .prepare("INSERT INTO items (label) VALUES (?)")
            .unwrap();
        for i in 1..=count {
            stmt.execute([format!("row-{i}")]).unwrap();
        }
    }

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "p".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(db_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    let creds: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    let clip = Arc::new(InMemoryClipboard::new());
    let clip_dyn: Arc<dyn Clipboard> = clip.clone();
    let mut core = AppCore::with_services(registry, connections, None, creds, clip_dyn);
    core.execute_command("open p");
    (core, clip, dir)
}

async fn preview_items(core: &mut AppCore) {
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl('w'));
    }
    // Connection (0) -> main schema (1) -> items (2).
    for _ in 0..2 {
        core.handle_key(key(KeyCode::Char('j')));
    }
    core.handle_key(key(KeyCode::Char('o')));
    core.drain_run_updates().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_and_prev_walk_through_pages() {
    let (mut core, _clip, _dir) = seeded(25).await;
    core.execute_command("page-size 10");
    assert!(core.status_message().contains("page size set to 10"));

    preview_items(&mut core).await;
    match core.result() {
        ResultState::Rows { rows, .. } => assert_eq!(rows.len(), 10),
        other => panic!("expected Rows, got {other:?}"),
    }

    core.execute_command("next");
    core.drain_run_updates().await;
    match core.result() {
        ResultState::Rows { rows, .. } => {
            assert_eq!(rows.len(), 10);
            // The first row of page 2 should be id=11.
            match &rows[0].0[0] {
                narwhal_core::Value::Int(n) => assert_eq!(*n, 11),
                other => panic!("expected int id, got {other:?}"),
            }
        }
        other => panic!("expected Rows, got {other:?}"),
    }

    core.execute_command("next");
    core.drain_run_updates().await;
    match core.result() {
        ResultState::Rows { rows, .. } => {
            // Last page: only 5 rows left.
            assert_eq!(rows.len(), 5);
            match &rows[0].0[0] {
                narwhal_core::Value::Int(n) => assert_eq!(*n, 21),
                other => panic!("expected int id, got {other:?}"),
            }
        }
        other => panic!("expected Rows, got {other:?}"),
    }

    core.execute_command("prev");
    core.drain_run_updates().await;
    match core.result() {
        ResultState::Rows { rows, .. } => {
            assert_eq!(rows.len(), 10);
            match &rows[0].0[0] {
                narwhal_core::Value::Int(n) => assert_eq!(*n, 11),
                other => panic!("expected int id, got {other:?}"),
            }
        }
        other => panic!("expected Rows, got {other:?}"),
    }

    core.execute_command("prev");
    core.drain_run_updates().await;
    core.execute_command("prev");
    assert!(core.status_message().contains("first page"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_without_preview_emits_status_only() {
    let (mut core, _clip, _dir) = seeded(5).await;
    core.execute_command("next");
    assert!(core.status_message().contains("no preview"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yank_cell_writes_value_to_clipboard() {
    let (mut core, clip, _dir) = seeded(3).await;
    preview_items(&mut core).await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w'));
    }
    core.handle_key(key(KeyCode::Char('j'))); // row 0
    core.handle_key(key(KeyCode::Char('l'))); // column 1 = label
    core.handle_key(key(KeyCode::Char('y')));

    assert!(core.status_message().starts_with("yanked"));
    assert_eq!(clip.read().as_deref(), Some("row-1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yank_row_writes_tsv_to_clipboard() {
    let (mut core, clip, _dir) = seeded(3).await;
    preview_items(&mut core).await;

    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w'));
    }
    core.handle_key(key(KeyCode::Char('j')));
    // Capital Y.
    core.handle_key(KeyEvent {
        code: KeyCode::Char('Y'),
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });

    assert!(core.status_message().starts_with("yanked row"));
    let pasted = clip.read().unwrap();
    // id and label, tab-separated.
    assert!(pasted.contains('\t'));
    assert!(pasted.ends_with("row-1"));
    assert!(pasted.starts_with("1\t"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn yank_without_result_emits_status_only() {
    let (mut core, _clip, _dir) = seeded(3).await;
    while core.focus() != Pane::Results {
        core.handle_key(ctrl('w'));
    }
    core.handle_key(key(KeyCode::Char('y')));
    assert!(core.status_message().contains("no cell"));
}
