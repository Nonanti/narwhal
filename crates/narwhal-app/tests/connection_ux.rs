//! `:url <dsn>`, `:edit <name>`, `:test <name|url>` end-to-end smoke.
//!
//! These tests exercise the three onboarding-UX commands without a
//! terminal: they drive [`AppCore::execute_command`] and inspect the
//! wizard / connections-file mirror state through the test-only
//! getters added in `core::mod`.

use std::path::PathBuf;

use narwhal_app::core::AppCore;
use narwhal_app::wizard::WizardFieldKind;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
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

/// `:url postgres://…` opens the wizard with every field already
/// hydrated from the parsed DSN. The user still has to press Enter
/// to commit, so `connections()` is unchanged at this point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn url_prefills_wizard_without_committing() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("url postgres://alice:s3cret@db.example.com:6432/inventory");

    let wizard = core.wizard().expect("wizard must be open after :url");
    let by = |kind: WizardFieldKind| -> String {
        wizard
            .fields
            .iter()
            .find(|f| f.kind == kind)
            .map(|f| f.value.expose().to_owned())
            .unwrap_or_default()
    };
    assert_eq!(wizard.driver(), "postgres");
    assert_eq!(by(WizardFieldKind::Host), "db.example.com");
    assert_eq!(by(WizardFieldKind::Port), "6432");
    assert_eq!(by(WizardFieldKind::Database), "inventory");
    assert_eq!(by(WizardFieldKind::Username), "alice");
    assert_eq!(by(WizardFieldKind::Password), "s3cret");
    // Wizard defaults name to the database when the DSN doesn't carry one.
    assert_eq!(by(WizardFieldKind::Name), "inventory");
    // No commit happened, so the persisted list is unchanged.
    assert_eq!(core.connections().len(), 1);
}

/// `:url <garbage>` surfaces a parse error in the status bar and
/// leaves the wizard closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn url_rejects_invalid_dsn() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("url not-a-url");
    assert!(
        core.wizard().is_none(),
        "wizard must stay closed on parse failure"
    );
    assert!(
        core.status_message().to_lowercase().contains("url"),
        "expected url error in status, got: {}",
        core.status_message()
    );
}

/// `:edit <name>` opens the wizard pre-populated from the saved
/// entry and carries `existing_id` so the commit path updates in
/// place instead of pushing a new row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_prefills_wizard_with_existing_id() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    rusqlite::Connection::open(&db_path).unwrap();
    let (registry, connections) = fixture(db_path.clone());
    let saved_id = connections.connections[0].id;
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("edit headless");
    let wizard = core.wizard().expect("wizard must open after :edit");
    assert_eq!(wizard.existing_id, Some(saved_id));
    assert_eq!(wizard.driver(), "sqlite");
    let name_field = wizard
        .fields
        .iter()
        .find(|f| f.kind == WizardFieldKind::Name)
        .expect("name field");
    assert_eq!(name_field.value.expose(), "headless");
}

/// `:edit nonexistent` reports the lookup miss and keeps the wizard closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_unknown_connection_emits_status_only() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("edit ghost");
    assert!(core.wizard().is_none());
    assert!(
        core.status_message().contains("ghost"),
        "expected the missing name in status, got: {}",
        core.status_message()
    );
}

/// `:test headless` opens a transient sqlite session and closes it —
/// the persistent `session()` slot must stay empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_named_connection_does_not_persist_session() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    rusqlite::Connection::open(&db_path).unwrap();
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("test headless");
    assert!(
        core.status_message().contains("test ok"),
        "expected success in status, got: {}",
        core.status_message()
    );
    assert!(core.session().is_none(), ":test must not leave a session");
}

/// `:test <bad-url>` short-circuits on the parser and never
/// touches the driver.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_invalid_url_reports_parse_error() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("test http://wrong-scheme/db");
    assert!(
        core.status_message().contains("test"),
        "expected test prefix in status, got: {}",
        core.status_message()
    );
    assert!(core.session().is_none());
}

/// `:test` with no argument and no active session emits a helpful hint
/// rather than panicking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_without_session_or_arg_emits_hint() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("test");
    assert!(
        core.status_message().contains("no active connection"),
        "expected 'no active connection' hint, got: {}",
        core.status_message()
    );
}

/// Round-trip: `:url <dsn>` prefills the wizard, the user types a
/// distinct name, and committing through `commit_wizard` (via Enter
/// emulation) writes the row to `connections()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn url_then_enter_persists_row() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    // No on-disk connections.toml here — the in-memory mirror is enough
    // for the test, and commit_wizard's save step is a no-op when
    // `connections_path` is None.
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("url postgres://u@h/db");
    // The wizard placed the cursor on the driver row; jump to `name`
    // (field index 0 ⇒ focused = 1) and overwrite the default.
    {
        let wizard = core.wizard().expect("wizard open");
        assert_eq!(wizard.driver(), "postgres");
        assert!(wizard
            .fields
            .iter()
            .any(|f| f.kind == WizardFieldKind::Name));
    }
    // Drive the form through the public key handler so we exercise the
    // same path the TUI uses.
    let send = |core: &mut AppCore, code: KeyCode| {
        core.handle_key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    };
    // Move focus to the name field, clear the default, retype "prod".
    send(&mut core, KeyCode::Tab);
    // The default name is "db" (3 chars) — pop until empty.
    for _ in 0..16 {
        send(&mut core, KeyCode::Backspace);
    }
    for c in "prod".chars() {
        send(&mut core, KeyCode::Char(c));
    }
    send(&mut core, KeyCode::Enter);

    assert!(core.wizard().is_none(), "wizard must close after commit");
    assert_eq!(core.connections().len(), 2);
    assert!(core.connections().iter().any(|c| c.name == "prod"));
}
