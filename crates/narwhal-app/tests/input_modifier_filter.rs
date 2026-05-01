//! Regression tests: input modals must ignore Ctrl-modified Char keystrokes.
//!
//! CB-2: editor search prompt
//! CB-3: history modal filter
//! CB-4: connection wizard fields

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_history::{HistoryEntry, Journal};

use tempfile::TempDir;
use uuid::Uuid;

const fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn make_core() -> AppCore {
    AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    )
}

// --- CB-2: editor search prompt ---

/// Ctrl-C during an open search prompt must NOT append 'c' to the needle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_search_ignores_ctrl_modified_chars() {
    let mut core = make_core();
    core.insert_into_editor("hello world").await;

    // Open forward search via '/' key and type "foo".
    core.handle_key(key(KeyCode::Char('/'), KeyModifiers::NONE))
        .await;
    assert!(core.tabs()[core.active_tab()].editor_search().prompt_open);
    for c in "foo".chars() {
        core.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }
    assert_eq!(core.tabs()[core.active_tab()].editor_search().needle, "foo");

    // Press Ctrl-C — needle must remain "foo", not become "fooc".
    core.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(
        core.tabs()[core.active_tab()].editor_search().needle,
        "foo",
        "Ctrl-C must not append 'c' to the search needle"
    );

    // Ctrl-V must also be ignored.
    core.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(
        core.tabs()[core.active_tab()].editor_search().needle,
        "foo",
        "Ctrl-V must not append 'v' to the search needle"
    );
}

// --- CB-3: history modal filter ---

/// Ctrl-modified characters must not be appended to the history filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_modal_ignores_ctrl_modified_chars() {
    let dir = TempDir::new().unwrap();
    let journal_path = dir.path().join("history.jsonl");
    let journal = Journal::open(&journal_path).await.unwrap();
    journal
        .append(&HistoryEntry::success("SELECT alpha"))
        .await
        .unwrap();
    drop(journal);
    let journal = Arc::new(Journal::open(&journal_path).await.unwrap());

    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(":memory:".into());
            }),
        }],
    };
    let mut core = AppCore::new(registry, connections, Some(journal));
    core.open_history().await;
    core.drain_meta_updates().await;

    // Type "al" into the filter.
    for c in "al".chars() {
        core.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }
    let state = core.history_state().expect("modal open");
    assert_eq!(state.filter, "al");

    // Press Ctrl-A — filter must remain "al".
    core.handle_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL))
        .await;
    let state = core.history_state().expect("modal open");
    assert_eq!(
        state.filter, "al",
        "Ctrl-A must not append 'a' to history filter"
    );

    // Press Ctrl-D — filter must remain "al".
    core.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await;
    let state = core.history_state().expect("modal open");
    assert_eq!(
        state.filter, "al",
        "Ctrl-D must not append 'd' to history filter"
    );
}

// --- CB-4: connection wizard fields ---

/// Ctrl-modified characters must not be inserted into wizard input fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wizard_ignores_ctrl_modified_chars() {
    let mut core = make_core();
    // Open the wizard via :add command.
    core.execute_command("add").await;
    assert!(core.wizard().is_some(), "wizard must be open after :add");

    // Move focus to the first text field (index 1, past the driver row).
    core.handle_key(key(KeyCode::Tab, KeyModifiers::NONE)).await;

    // Type "db" normally.
    for c in "db".chars() {
        core.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }

    // The focused field should contain "db".
    let wizard = core.wizard().expect("wizard open");
    let focused_value = wizard.fields[wizard.focused - 1].value.expose().to_owned();
    assert!(
        focused_value.contains("db"),
        "expected 'db' in field, got: {focused_value}"
    );

    // Now press Ctrl-V — must NOT append 'v'.
    core.handle_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL))
        .await;
    let wizard = core.wizard().expect("wizard open");
    let focused_value = wizard.fields[wizard.focused - 1].value.expose().to_owned();
    assert!(
        !focused_value.contains('v'),
        "Ctrl-V must not append 'v' to wizard field, got: {focused_value}"
    );

    // Ctrl-C must also be ignored.
    core.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    let wizard = core.wizard().expect("wizard open");
    let focused_value = wizard.fields[wizard.focused - 1].value.expose().to_owned();
    assert_eq!(
        focused_value, "db",
        "Ctrl-C must not append 'c' to wizard field"
    );
}
