//! Integration tests that exercise [`AppCore`] without a terminal.
//!
//! The tests open an in-memory SQLite session, dispatch commands and
//! verify the resulting [`ResultState`].

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::{AppCore, ResultState};
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
            name: "headless".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_then_run_returns_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('first'), ('second'), ('third');",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    assert!(matches!(core.result(), ResultState::Empty));
    assert!(core.session().is_none());

    core.execute_command("open headless");
    assert!(core.session().is_some(), "session must open");

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { columns, rows, .. } => {
            assert_eq!(columns.len(), 2);
            assert_eq!(rows.len(), 3);
        }
        other => panic!("expected Rows, got {other:?}"),
    }
    assert!(core.status_message().contains("done · 1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_all_executes_every_statement() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open headless");
    core.insert_into_editor(
        "CREATE TABLE notes (n TEXT); \
         INSERT INTO notes VALUES ('alpha'); \
         INSERT INTO notes VALUES ('beta'); \
         SELECT * FROM notes",
    );
    core.execute_command("run-all");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected Rows, got {other:?}"),
    }
    assert!(core.status_message().contains("done · 4"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_connection_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open does-not-exist");
    assert!(core.session().is_none());
    assert!(core.status_message().contains("connection not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_enter_opens_table_detail() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("detail.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE);
             CREATE TABLE orders (
                 id INTEGER PRIMARY KEY,
                 customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
                 placed_at TEXT NOT NULL
             );",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless");

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
    // Step down to the `orders` row: connection (0) -> main (1) ->
    // customers (2) -> orders (3).
    let j = KeyEvent {
        code: KeyCode::Char('j'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    for _ in 0..3 {
        core.handle_key(j);
    }
    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(enter);

    match core.result() {
        ResultState::TableDetail { schema } => {
            assert_eq!(schema.table.name, "orders");
            assert_eq!(schema.columns.len(), 3);
            assert_eq!(schema.foreign_keys.len(), 1);
            assert_eq!(schema.foreign_keys[0].referenced_table, "customers");
        }
        other => panic!("expected TableDetail, got {other:?}"),
    }
}

/// Pressing `:` from the sidebar (or any non-editor pane) should snap
/// focus back to the editor and enter vim command mode, so the user can
/// type `:open <conn>` without first cycling panes with Ctrl-W.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn colon_from_sidebar_focuses_editor_and_enters_command_mode() {
    use narwhal_vim::Mode;

    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);

    // Cycle focus to the sidebar.
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl_w);
    }
    assert_eq!(core.focus(), Pane::Sidebar);

    // Type `:` — should jump focus to the editor and arm command mode.
    let colon = KeyEvent {
        code: KeyCode::Char(':'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    core.handle_key(colon);
    assert_eq!(core.focus(), Pane::Editor);
    assert_eq!(core.mode(), Mode::Command);
    assert_eq!(core.command_buffer(), "");

    // Subsequent letters accumulate in the command buffer.
    for c in "open".chars() {
        core.handle_key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }
    assert_eq!(core.command_buffer(), "open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_without_session_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("run");
    assert!(core.status_message().contains("no active connection"));
    assert!(matches!(core.result(), ResultState::Empty));
}

/// Pin the active-tab invariant relied on by `AppCore::tab()` /
/// `AppCore::tab_mut()`: `tabs.len() >= 1` and
/// `active_tab < tabs.len()` at every public-API exit point.
///
/// Drives the four state-mutating tab commands (`:new`, `:tabclose`,
/// `:tabnext`, `:tabprev`) across a small mix of tab counts and
/// asserts the invariant after each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_tab_invariant_holds_across_lifecycle() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);

    let check = |core: &AppCore, where_: &str| {
        let len = core.tabs().len();
        assert!(len >= 1, "{where_}: tabs.len() == 0");
        assert!(
            core.active_tab() < len,
            "{where_}: active_tab {} >= tabs.len() {}",
            core.active_tab(),
            len
        );
    };

    check(&core, "fresh AppCore");

    core.execute_command("new");
    core.execute_command("new");
    core.execute_command("new");
    check(&core, "after :new x3");
    assert_eq!(core.tabs().len(), 4);

    core.execute_command("tabnext");
    core.execute_command("tabnext");
    core.execute_command("tabnext");
    core.execute_command("tabnext");
    check(&core, "after :tabnext x4 (wrap)");

    core.execute_command("tabprev");
    core.execute_command("tabprev");
    check(&core, "after :tabprev x2");

    core.execute_command("tabclose");
    core.execute_command("tabclose");
    core.execute_command("tabclose");
    check(&core, "after :tabclose x3");
    assert_eq!(core.tabs().len(), 1);

    // Closing the last tab is a no-op with a status message.
    core.execute_command("tabclose");
    check(&core, "after attempted close of last tab");
    assert_eq!(core.tabs().len(), 1);
    assert!(core.status_message().contains("last tab"));
}

/// The connection slot in the status bar is sticky: it survives
/// transient messages produced by queries, searches, etc.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_bar_pins_connection_through_transient_messages() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items (label) VALUES ('alpha'), ('beta');",
        )
        .unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    // No connection yet.
    assert!(core.status_bar().connection.is_none());

    // Open a connection — the center slot must be populated.
    core.execute_command("open headless");
    let conn_slot = core
        .status_bar()
        .connection
        .clone()
        .expect("connection slot should be set after :open");
    assert!(
        conn_slot.contains("headless"),
        "connection slot should contain the connection name, got: {conn_slot}"
    );

    // Run a query — produces a transient message but must NOT clear the
    // connection slot.
    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    assert!(
        core.status_bar().connection.is_some(),
        "connection slot must survive transient messages"
    );
    assert!(
        core.status_bar()
            .connection
            .as_ref()
            .unwrap()
            .contains("headless"),
        "connection slot still names headless after query"
    );
}

/// Closing a session clears the connection slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_bar_clears_connection_on_close() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    // No connection initially.
    assert!(core.status_bar().connection.is_none());

    // Open — slot appears.
    core.execute_command("open headless");
    assert!(
        core.status_bar().connection.is_some(),
        "connection slot must be set after :open"
    );

    // Close — slot clears.
    core.execute_command("close");
    assert!(
        core.status_bar().connection.is_none(),
        "connection slot must be cleared after :close"
    );
}
