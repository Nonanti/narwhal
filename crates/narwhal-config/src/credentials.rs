use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredentialError {
    #[error("credential not found")]
    NotFound,
    #[error("keyring error: {0}")]
    Keyring(String),
}

impl From<keyring::Error> for CredentialError {
    fn from(error: keyring::Error) -> Self {
        match error {
            keyring::Error::NoEntry => Self::NotFound,
            other => Self::Keyring(other.to_string()),
        }
    }
}

/// Storage abstraction for connection secrets.
///
/// Concrete implementations include [`KeyringStore`], which delegates to the
/// operating-system credential service, and lightweight in-memory variants
/// used in tests.
///
/// All methods are async so that implementations performing blocking I/O
/// (such as OS keyring D-Bus calls) can offload to [`tokio::task::spawn_blocking`]
/// without stalling the async runtime.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, connection_id: Uuid) -> Result<Option<SecretString>, CredentialError>;
    async fn set(&self, connection_id: Uuid, secret: SecretString) -> Result<(), CredentialError>;
    async fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError>;
}

const SERVICE: &str = "narwhal";

/// Credential store backed by the operating-system keyring.
///
/// Blocking keyring calls are wrapped in [`tokio::task::spawn_blocking`] so
/// that the async runtime is never stalled by D-Bus / Secret-Service I/O.
#[derive(Debug, Default)]
pub struct KeyringStore;

impl KeyringStore {
    pub const fn new() -> Self {
        Self
    }

    fn entry(connection_id: Uuid) -> Result<keyring::Entry, CredentialError> {
        let account = connection_id.to_string();
        keyring::Entry::new(SERVICE, &account).map_err(Into::into)
    }

    fn get_blocking(connection_id: Uuid) -> Result<Option<String>, CredentialError> {
        match Self::entry(connection_id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set_blocking(connection_id: Uuid, secret: &str) -> Result<(), CredentialError> {
        Self::entry(connection_id)?.set_password(secret)?;
        Ok(())
    }

    fn delete_blocking(connection_id: Uuid) -> Result<(), CredentialError> {
        match Self::entry(connection_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl CredentialStore for KeyringStore {
    async fn get(&self, connection_id: Uuid) -> Result<Option<SecretString>, CredentialError> {
        let id = connection_id;
        tokio::task::spawn_blocking(move || Self::get_blocking(id))
            .await
            .map_err(|e| CredentialError::Keyring(format!("spawn_blocking join error: {e}")))?
            .map(|opt| opt.map(|s| secrecy::SecretString::new(s.into_boxed_str())))
    }

    async fn set(&self, connection_id: Uuid, secret: SecretString) -> Result<(), CredentialError> {
        let id = connection_id;
        // We must extract the secret for the blocking closure. The keyring
        // crate takes &str, so we expose it here — the only place the
        // secret material is read outside of its protective wrapper.
        // The extracted String is zeroized after use to avoid leaving
        // plaintext credentials on the heap.
        let mut plain = secret.expose_secret().to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let res = Self::set_blocking(id, &plain);
            plain.zeroize();
            res
        })
        .await
        .map_err(|e| CredentialError::Keyring(format!("spawn_blocking join error: {e}")))?;
        result
    }

    async fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        let id = connection_id;
        tokio::task::spawn_blocking(move || Self::delete_blocking(id))
            .await
            .map_err(|e| CredentialError::Keyring(format!("spawn_blocking join error: {e}")))?
    }
}

/// In-memory credential store. Used by tests and as a transparent fallback
/// when no OS keyring is available (e.g. headless CI).
#[derive(Debug, Default)]
pub struct InMemoryStore {
    secrets: Mutex<HashMap<Uuid, SecretString>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CredentialStore for InMemoryStore {
    async fn get(&self, connection_id: Uuid) -> Result<Option<SecretString>, CredentialError> {
        let guard = self
            .secrets
            .lock()
            .map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))?;
        Ok(guard.get(&connection_id).cloned())
    }

    async fn set(&self, connection_id: Uuid, secret: SecretString) -> Result<(), CredentialError> {
        let mut guard = self
            .secrets
            .lock()
            .map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))?;
        guard.insert(connection_id, secret);
        Ok(())
    }

    async fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        let mut guard = self
            .secrets
            .lock()
            .map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))?;
        guard.remove(&connection_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn in_memory_round_trip() {
        let store = InMemoryStore::new();
        let id = Uuid::new_v4();
        assert!(store.get(id).await.unwrap().is_none());
        store
            .set(id, SecretString::new("s3cret".into()))
            .await
            .unwrap();
        assert_eq!(
            store
                .get(id)
                .await
                .unwrap()
                .as_ref()
                .map(|s| s.expose_secret() as &str),
            Some("s3cret")
        );
        store.delete(id).await.unwrap();
        assert!(store.get(id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_delete_missing_is_ok() {
        let store = InMemoryStore::new();
        store.delete(Uuid::new_v4()).await.unwrap();
    }

    /// Regression: the `keyring` crate (>= 3.x) ships zero default
    /// features and falls back to a mock backend that accepts `set()`
    /// and returns `None` from `get()`.  If our Cargo.toml ever loses
    /// the platform-native / secret-service feature flags, every
    /// production install would silently drop saved passwords on the
    /// floor.  This test pins the workspace by asserting that the
    /// default credential builder is NOT the mock one — the mock
    /// reports `CredentialPersistence::EntryOnly`, every real backend
    /// reports `UntilDelete` or `UntilReboot`.
    #[test]
    fn keyring_backend_is_not_mock() {
        use keyring::credential::CredentialPersistence;

        // `keyring::default` is `pub use mock as default` when no
        // backend feature is enabled, so the persistence reported by
        // its credential builder is `EntryOnly` — the smoking-gun for
        // a misconfigured Cargo.toml.  Any real backend (linux-native,
        // sync-secret-service, apple-native, windows-native) reports
        // `UntilDelete` or `UntilReboot`.
        let persistence = keyring::default::default_credential_builder().persistence();
        assert!(
            !matches!(persistence, CredentialPersistence::EntryOnly),
            "keyring crate compiled WITHOUT a real backend feature \
             flag — saved passwords would be silently lost. Enable \
             one of: apple-native, windows-native, sync-secret-service, \
             linux-native in workspace Cargo.toml."
        );
    }

    /// H8 regression: `KeyringStore` offloads to `spawn_blocking` so the
    /// async runtime is never blocked. We verify the async API compiles
    /// and the `InMemoryStore` returns the correct type.
    #[tokio::test]
    async fn credential_store_trait_is_async() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryStore::new());
        let id = Uuid::new_v4();
        // set requires SecretString, not &str
        store.set(id, SecretString::new("pw".into())).await.unwrap();
        // get returns Option<SecretString>
        let got = store.get(id).await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.as_ref().unwrap().expose_secret(), "pw");
        // delete
        store.delete(id).await.unwrap();
        assert!(store.get(id).await.unwrap().is_none());
    }
}
