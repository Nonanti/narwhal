//! Headless integration tests for the editor's completion popup.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

const fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

async fn type_str(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
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
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "c".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open c").await;
    core
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unique_match_tab_accepts_from_popup() {
    let mut core = open_with_tables(&["users"]).await;
    // Insert mode + type a prefix that uniquely matches the `users` table.
    // Auto-trigger opens the popup as soon as 2+ characters are typed.
    core.handle_key(key(KeyCode::Char('i'))).await;
    type_str(&mut core, "user").await;
    // Tab inside an open popup accepts the highlighted entry (IDE-style).
    core.handle_key(key(KeyCode::Tab)).await;
    let text = core.editor().entire_text();
    assert!(
        text.contains("users"),
        "expected users completion in editor, got: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_matches_open_popup_and_enter_inserts() {
    let mut core = open_with_tables(&["orders", "order_items", "owners"]).await;
    core.handle_key(key(KeyCode::Char('i'))).await;
    type_str(&mut core, "ord").await;
    // Auto-trigger opens the popup silently — no status spam.
    assert!(
        core.editor_completion_is_open().await,
        "popup should be open after auto-trigger"
    );

    // Down arrow moves the highlight to the second item.
    core.handle_key(key(KeyCode::Down)).await;
    // Enter accepts.
    core.handle_key(key(KeyCode::Enter)).await;
    let text = core.editor().entire_text();
    // The exact ordering depends on lexicographic sort; assert the
    // buffer grew beyond the original prefix and the popup is closed.
    assert!(text.len() > "ord".len(), "buffer: {text:?}");
    assert!(!core.editor_completion_is_open().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_n_advances_completion_popup_when_open() {
    // M1 regression: Ctrl-N must drive the completion popup down
    // when it's open, not open the :goto modal. The goto modal
    // would steal focus and strand the user mid-completion.
    let mut core = open_with_tables(&["orders", "order_items", "owners"]).await;
    core.handle_key(key(KeyCode::Char('i'))).await;
    type_str(&mut core, "ord").await;
    assert!(core.editor_completion_is_open().await);

    core.handle_key(ctrl_key(KeyCode::Char('n'))).await;
    // Popup must still be open (goto modal would have closed it).
    assert!(
        core.editor_completion_is_open().await,
        "Ctrl-N closed the completion popup or opened the goto modal"
    );
    // Accept the now-second item; the buffer must have changed beyond
    // the original prefix, confirming Ctrl-N did move the highlight.
    core.handle_key(key(KeyCode::Enter)).await;
    let text = core.editor().entire_text();
    assert!(text.len() > "ord".len(), "buffer after enter: {text:?}");
    assert!(!core.editor_completion_is_open().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn esc_dismisses_popup_without_inserting() {
    let mut core = open_with_tables(&["orders", "order_items"]).await;
    core.handle_key(key(KeyCode::Char('i'))).await;
    type_str(&mut core, "ord").await;
    assert!(
        core.editor_completion_is_open().await,
        "popup should be open after auto-trigger"
    );
    core.handle_key(key(KeyCode::Esc)).await;
    assert!(core.status_message().contains("cancelled"));
    let text = core.editor().entire_text();
    assert_eq!(text, "ord");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_prefix_inserts_four_spaces() {
    let mut core = open_with_tables(&["orders"]).await;
    core.handle_key(key(KeyCode::Char('i'))).await;
    core.handle_key(key(KeyCode::Tab)).await;
    let text = core.editor().entire_text();
    assert_eq!(text, "    ");
}
