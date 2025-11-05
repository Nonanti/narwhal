//! Integration tests for the Ctrl+R history modal.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_history::{HistoryEntry, Journal};
use tempfile::TempDir;
use uuid::Uuid;

#[allow(dead_code)]
fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Seed the journal with entries and reopen it so the data is on disk.
async fn seed_journal(dir: &TempDir, entries: Vec<HistoryEntry>) -> Arc<Journal> {
    let journal_path = dir.path().join("history.jsonl");
    let journal = Journal::open(&journal_path).await.unwrap();
    for entry in &entries {
        journal.append(entry).await.unwrap();
    }
    drop(journal);
    Arc::new(Journal::open(&journal_path).await.unwrap())
}

/// 1. Opening the history modal loads entries from the journal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_opens_with_journal_entries() {
    let dir = TempDir::new().unwrap();
    let journal = seed_journal(
        &dir,
        vec![
            HistoryEntry::success("SELECT 1"),
            HistoryEntry::success("SELECT 2"),
            HistoryEntry::success("SELECT 3"),
        ],
    )
    .await;

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));

    assert!(!core.history_is_open());
    core.open_history();

    let state = core.history_state().expect("modal should be open");
    assert_eq!(state.entries.len(), 3);
}

/// 2. Typing a filter substring narrows the visible entries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_filter_narrows_visible() {
    let dir = TempDir::new().unwrap();
    let journal = seed_journal(
        &dir,
        vec![
            HistoryEntry::success("SELECT alpha"),
            HistoryEntry::success("SELECT beta"),
            HistoryEntry::success("INSERT alpha"),
        ],
    )
    .await;

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));

    core.open_history();
    // Type "alpha" into the filter.
    for c in "alpha".chars() {
        core.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }

    let state = core.history_state().expect("modal should be open");
    let visible = state.visible_entries();
    assert_eq!(visible.len(), 2); // "SELECT alpha" and "INSERT alpha"
}

/// 3. Enter inserts the selected SQL into the editor and closes the modal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_enter_inserts_sql_into_editor() {
    let dir = TempDir::new().unwrap();
    let journal = seed_journal(
        &dir,
        vec![
            HistoryEntry::success("SELECT 1"),
            HistoryEntry::success("SELECT hello_world"),
        ],
    )
    .await;

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));

    core.open_history();
    // Most recent is "SELECT hello_world" (newest first).
    core.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!core.history_is_open(), "modal should close on Enter");
    let text = core.editor().entire_text();
    assert!(
        text.contains("SELECT hello_world"),
        "editor should contain the inserted SQL, got: {text}"
    );
}

/// 4. Esc closes the modal without changing the editor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_esc_closes_without_change() {
    let dir = TempDir::new().unwrap();
    let journal = seed_journal(&dir, vec![HistoryEntry::success("SELECT 1")]).await;

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));

    let text_before = core.editor().entire_text().clone();
    core.open_history();
    assert!(core.history_is_open());

    core.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!core.history_is_open(), "modal should close on Esc");
    assert_eq!(
        core.editor().entire_text(),
        text_before,
        "editor should be unchanged after Esc"
    );
}

/// 5. Without a journal, open_history shows a status message and
///    `history_state` stays None.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_no_journal_shows_message() {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(":memory:".into()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, None);

    core.open_history();
    assert!(
        !core.history_is_open(),
        "modal should NOT open without a journal"
    );
    assert!(
        core.status_message().contains("history disabled"),
        "status should explain why, got: {}",
        core.status_message()
    );
}
