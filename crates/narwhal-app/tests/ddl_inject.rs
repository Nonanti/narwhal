//! Integration test: pressing `d` with a sidebar table focused
//! injects DDL into the editor.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::Pane;
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "ddl-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d_on_sidebar_table_injects_ddl() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);")
            .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    // Open the connection.
    core.execute_command("open ddl-test");
    assert!(core.session().is_some(), "session must open");

    // Switch focus to the sidebar (Ctrl-W cycles editor->results->sidebar).
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl_w);
    }

    // Walk down the sidebar until we land on the 'items' table row.
    // Layout: connection (0) -> main (1) -> items (2).
    for _ in 0..2 {
        core.handle_key(key(KeyCode::Down));
    }

    // Press 'd' to fetch DDL.
    core.handle_key(key(KeyCode::Char('d')));

    // The editor should now contain the DDL.
    let editor_text = core.editor().entire_text();
    assert!(
        editor_text.contains("items"),
        "editor should contain table name after DDL inject, got: {editor_text}"
    );
    assert!(
        editor_text.contains("id"),
        "editor should contain column 'id' after DDL inject, got: {editor_text}"
    );
    assert!(
        editor_text.contains("label"),
        "editor should contain column 'label' after DDL inject, got: {editor_text}"
    );

    // Focus should have moved to the editor.
    assert_eq!(
        core.focus(),
        Pane::Editor,
        "focus should move to editor after DDL inject"
    );

    // Status message should mention the injection.
    let msg = core.status_message();
    assert!(
        msg.contains("injected DDL"),
        "status should mention DDL injection, got: {msg}"
    );
}
