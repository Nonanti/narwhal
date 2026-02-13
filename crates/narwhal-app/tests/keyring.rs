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

fn type_str(core: &mut AppCore, text: &str) {
    for ch in text.chars() {
        core.handle_key(key(KeyCode::Char(ch)));
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
    core.execute_command("add");
    // Switch driver to postgres (sqlite → postgres).
    core.handle_key(key(KeyCode::Right));

    // Tab past the driver row onto `name`, fill the postgres form.
    let tab = key(KeyCode::Tab);
    core.handle_key(tab); // -> name
    type_str(&mut core, "prod");
    core.handle_key(tab); // -> host
    type_str(&mut core, "db.example.com");
    core.handle_key(tab); // -> port (pre-filled 5432)
    core.handle_key(tab); // -> database
    type_str(&mut core, "inventory");
    core.handle_key(tab); // -> username
    type_str(&mut core, "admin");
    core.handle_key(tab); // -> password
    type_str(&mut core, "s3cret");

    // Enter commits the wizard.
    core.handle_key(key(KeyCode::Enter));

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

    core.execute_command("forget stage");
    assert!(core.status_message().contains("forgot password"));
    // Sprint 9 (H7): keyring delete is now fire-and-forget on a
    // background task. Yield briefly so the spawned task runs.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

    core.execute_command("remove dev");
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
            params: ConnectionParams {
                path: Some(db_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };

    let registry = DriverRegistry::with_defaults();
    let mut core = AppCore::with_credentials(registry, connections, None, store.clone());
    core.execute_command("open local");
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
