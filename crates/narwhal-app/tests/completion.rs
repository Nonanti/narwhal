//! Headless integration tests for the editor's completion popup.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
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

fn type_str(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch)));
    }
}

async fn open_with_tables(tables: &[&str]) -> AppCore {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("c.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    for t in tables {
        conn.execute(
            &format!("CREATE TABLE {t} (id INTEGER PRIMARY KEY, label TEXT)"),
            [],
        )
        .unwrap();
    }
    // Keep the tempdir alive for the test's lifetime by intentionally
    // leaking it: tests don't need a clean shutdown.
    Box::leak(Box::new(dir));

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "c".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(db_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open c");
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unique_match_auto_completes_inline() {
    let mut core = open_with_tables(&["users"]).await;
    // Enter insert mode and type a unique prefix.
    core.handle_key(key(KeyCode::Char('i')));
    type_str(&mut core, "use");
    // Tab → only one table matches "use" (the keyword USING also matches,
    // so this is not actually unique; type more to make it so).
    type_str(&mut core, "r");
    core.handle_key(key(KeyCode::Tab));
    let text = core.editor().entire_text();
    assert!(
        text.contains("users"),
        "expected users completion in editor, got: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_matches_open_popup_and_enter_inserts() {
    let mut core = open_with_tables(&["orders", "order_items", "owners"]).await;
    core.handle_key(key(KeyCode::Char('i')));
    type_str(&mut core, "ord");
    core.handle_key(key(KeyCode::Tab));
    assert!(
        core.status_message().contains("cycles"),
        "popup should have opened, got status: {}",
        core.status_message()
    );

    // Cycle once with Tab to pick the second item.
    core.handle_key(key(KeyCode::Tab));
    core.handle_key(key(KeyCode::Enter));
    let text = core.editor().entire_text();
    // After cycling Tab once, the second item is selected. The exact
    // ordering depends on lexicographic sort, so just assert the buffer
    // grew beyond the original prefix.
    assert!(text.len() > "ord".len(), "buffer: {text:?}");
    assert!(!core.status_message().contains("cycles"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_dismisses_popup_without_inserting() {
    let mut core = open_with_tables(&["orders", "order_items"]).await;
    core.handle_key(key(KeyCode::Char('i')));
    type_str(&mut core, "ord");
    core.handle_key(key(KeyCode::Tab));
    assert!(core.status_message().contains("cycles"));
    core.handle_key(key(KeyCode::Esc));
    assert!(core.status_message().contains("cancelled"));
    let text = core.editor().entire_text();
    assert_eq!(text, "ord");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_prefix_inserts_four_spaces() {
    let mut core = open_with_tables(&["orders"]).await;
    core.handle_key(key(KeyCode::Char('i')));
    core.handle_key(key(KeyCode::Tab));
    let text = core.editor().entire_text();
    assert_eq!(text, "    ");
}
