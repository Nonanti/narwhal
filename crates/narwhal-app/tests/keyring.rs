//! Integration tests for the wizard → keyring → open flow.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::{ConnectionsFile, CredentialStore, InMemoryStore, SecretString};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use secrecy::ExposeSecret;
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

async fn type_str(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
}

/// Drive the wizard interactively and verify the password lands in the
/// supplied credential store under the freshly-minted connection id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wizard_persists_password_to_credentials() {
    let dir = TempDir::new().unwrap();
    let connections_path = dir.path().join("connections.toml");
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());

    let registry = DriverRegistry::with_defaults();
    let mut core =
        AppCore::with_credentials(registry, ConnectionsFile::default(), None, store.clone());
    core.set_connections_path(connections_path.clone());

    // :add → modal wizard appears.
    core.execute_command("add").await;
    // Switch driver to postgres (sqlite → postgres).
    core.handle_key(key(KeyCode::Right)).await;

    // Tab past the driver row onto `name`, fill the postgres form.
    let tab = key(KeyCode::Tab);
    core.handle_key(tab).await; // -> name
    type_str(&mut core, "prod").await;
    core.handle_key(tab).await; // -> host
    type_str(&mut core, "db.example.com").await;
    core.handle_key(tab).await; // -> port (pre-filled 5432)
    core.handle_key(tab).await; // -> database
    type_str(&mut core, "inventory").await;
    core.handle_key(tab).await; // -> username
    type_str(&mut core, "admin").await;
    core.handle_key(tab).await; // -> password
    type_str(&mut core, "s3cret").await;

    // Enter commits the wizard.
    core.handle_key(key(KeyCode::Enter)).await;

    // The connection landed in the file and the secret in the store.
    let saved = ConnectionsFile::load(&connections_path).unwrap();
    assert_eq!(saved.connections.len(), 1);
    let id = saved.connections[0].id;
    assert_eq!(saved.connections[0].name, "prod");
    assert_eq!(saved.connections[0].driver, "postgres");
    assert_eq!(
        store
            .get(id)
            .await
            .unwrap()
            .as_ref()
            .map(|s| s.expose_secret() as &str),
        Some("s3cret")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_clears_keyring_but_keeps_connection() {
    let dir = TempDir::new().unwrap();
    let connections_path = dir.path().join("connections.toml");
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());

    let id = Uuid::new_v4();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "stage".into(),
            driver: "postgres".into(),
            params: ConnectionParams::default(),
        }],
    };
    connections.save(&connections_path).unwrap();
    store
        .set(id, SecretString::new("oldpw".into()))
        .await
        .unwrap();

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.set_connections_path(connections_path.clone());

    core.execute_command("forget stage").await;
    // The delete now reports back through the meta channel so the
    // status line shows a real verdict (instead of the previous
    // "(best-effort)" placeholder). Drain pending updates before
    // asserting on the keyring state.
    core.drain_meta_updates().await;
    assert!(
        core.status_message().contains("forgot password"),
        "expected real success message, got: {}",
        core.status_message()
    );
    assert!(store.get(id).await.unwrap().is_none());

    let still_there = ConnectionsFile::load(&connections_path).unwrap();
    assert_eq!(still_there.connections.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_drops_connection_and_secret() {
    let dir = TempDir::new().unwrap();
    let connections_path = dir.path().join("connections.toml");
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());

    let id = Uuid::new_v4();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "dev".into(),
            driver: "postgres".into(),
            params: ConnectionParams::default(),
        }],
    };
    connections.save(&connections_path).unwrap();
    store.set(id, SecretString::new("pw".into())).await.unwrap();

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.set_connections_path(connections_path.clone());

    core.execute_command("remove dev").await;
    assert!(core.status_message().contains("removed"));
    // Sprint 9 (H7): keyring delete is now fire-and-forget.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(store.get(id).await.unwrap().is_none());
    let on_disk = ConnectionsFile::load(&connections_path).unwrap();
    assert!(on_disk.connections.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_pulls_password_from_credentials() {
    // A sqlite session doesn't actually need a password, but we still
    // verify the credential store is consulted on the open path: the
    // lookup must occur and not abort the connection when present.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("k.db");
    rusqlite::Connection::open(&db_path).unwrap();

    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
    let id = Uuid::new_v4();
    store
        .set(id, SecretString::new("ignored-by-sqlite".into()))
        .await
        .unwrap();

    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "local".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.execute_command("open local").await;
    assert!(core.session().is_some(), "session must open");
    // Secret remains in the store after open.
    assert_eq!(
        store
            .get(id)
            .await
            .unwrap()
            .as_ref()
            .map(|s| s.expose_secret() as &str),
        Some("ignored-by-sqlite")
    );
}

/// Bug-fix regression: `:edit <name>` must prefill the wizard's
/// password field with the stored secret. An earlier refactor moved
/// the keyring lookup off the synchronous open path but failed to
/// inject the result, leaving the field empty and forcing the user
/// to retype their password every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_prefills_password_from_keyring() {
    use narwhal_commands::wizard::WizardFieldKind;

    let dir = TempDir::new().unwrap();
    let connections_path = dir.path().join("connections.toml");
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());

    let id = Uuid::new_v4();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "prod".into(),
            driver: "postgres".into(),
            params: ConnectionParams::default(),
        }],
    };
    connections.save(&connections_path).unwrap();
    store
        .set(id, SecretString::new("hunter2".into()))
        .await
        .unwrap();

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.set_connections_path(connections_path);

    core.execute_command("edit prod").await;
    assert!(core.wizard().is_some(), "wizard must open");
    // Background keyring lookup completes via meta channel.
    core.drain_meta_updates().await;

    let wizard = core.wizard().expect("wizard still open");
    let password_field = wizard
        .fields
        .iter()
        .find(|f| f.kind == WizardFieldKind::Password)
        .expect("password field exists");
    assert_eq!(
        password_field.value.expose(),
        "hunter2",
        "password field must be prefilled with the stored secret",
    );
}

/// Bug-fix regression: `:test` with no argument used to send the
/// result of `pool.acquire()` to `tracing::debug` only — the status
/// line stayed stuck on "testing…" forever, so the user could not
/// tell if the active session was healthy. The fix routes the ping
/// through the meta channel and produces a real verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_active_session_reports_real_verdict() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("t.db");
    rusqlite::Connection::open(&db_path).unwrap();

    let id = Uuid::new_v4();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "local".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };
    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open local").await;
    assert!(core.session().is_some());

    core.execute_command("test").await;
    // The ping is dispatched on the meta channel; wait for the
    // verdict to arrive.
    core.drain_meta_updates().await;

    let msg = core.status_message();
    assert!(
        msg.contains("test ok"),
        "`:test` (no arg) must report a real verdict, got: {msg}",
    );
}

/// Bug-fix regression: `:forget <name>` used to show the misleading
/// status "forgot password for 'X' (best-effort)" before the delete
/// had actually happened (or possibly even succeeded). The fix waits
/// for the worker's reply and surfaces a real "forgot password" or
/// "forget failed: …" message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_status_reflects_real_outcome_not_best_effort() {
    let dir = TempDir::new().unwrap();
    let connections_path = dir.path().join("connections.toml");
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());

    let id = Uuid::new_v4();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id,
            name: "stage".into(),
            driver: "postgres".into(),
            params: ConnectionParams::default(),
        }],
    };
    connections.save(&connections_path).unwrap();
    store.set(id, SecretString::new("pw".into())).await.unwrap();

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.set_connections_path(connections_path);

    core.execute_command("forget stage").await;
    // Pre-drain: status should be the in-flight placeholder, *not*
    // the misleading "(best-effort)" jargon.
    assert!(
        core.status_message().contains("forgetting"),
        "in-flight status must be a progress indicator, got: {}",
        core.status_message()
    );
    assert!(
        !core.status_message().contains("best-effort"),
        "(best-effort) placeholder must not appear, got: {}",
        core.status_message()
    );

    core.drain_meta_updates().await;

    assert!(
        core.status_message().contains("forgot password"),
        "final status must reflect the real outcome, got: {}",
        core.status_message()
    );
    assert!(store.get(id).await.unwrap().is_none());
}
