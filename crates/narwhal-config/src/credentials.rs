use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
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
pub trait CredentialStore: Send + Sync {
    fn get(&self, connection_id: Uuid) -> Result<Option<String>, CredentialError>;
    fn set(&self, connection_id: Uuid, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError>;
}

const SERVICE: &str = "narwhal";

/// Credential store backed by the operating-system keyring.
#[derive(Debug, Default)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(connection_id: Uuid) -> Result<keyring::Entry, CredentialError> {
        let account = connection_id.to_string();
        keyring::Entry::new(SERVICE, &account).map_err(Into::into)
    }
}

impl CredentialStore for KeyringStore {
    fn get(&self, connection_id: Uuid) -> Result<Option<String>, CredentialError> {
        match Self::entry(connection_id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, connection_id: Uuid, secret: &str) -> Result<(), CredentialError> {
        Self::entry(connection_id)?.set_password(secret)?;
        Ok(())
    }

    fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        match Self::entry(connection_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// In-memory credential store. Used by tests and as a transparent fallback
/// when no OS keyring is available (e.g. headless CI).
#[derive(Debug, Default)]
pub struct InMemoryStore {
    secrets: Mutex<HashMap<Uuid, String>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryStore {
    fn get(&self, connection_id: Uuid) -> Result<Option<String>, CredentialError> {
        let guard = self
            .secrets
            .lock()
            .map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))?;
        Ok(guard.get(&connection_id).cloned())
    }

    fn set(&self, connection_id: Uuid, secret: &str) -> Result<(), CredentialError> {
        let mut guard = self
            .secrets
            .lock()
            .map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))?;
        guard.insert(connection_id, secret.to_owned());
        Ok(())
    }

    fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
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

    #[test]
    fn in_memory_round_trip() {
        let store = InMemoryStore::new();
        let id = Uuid::new_v4();
        assert!(store.get(id).unwrap().is_none());
        store.set(id, "s3cret").unwrap();
        assert_eq!(store.get(id).unwrap().as_deref(), Some("s3cret"));
        store.delete(id).unwrap();
        assert!(store.get(id).unwrap().is_none());
    }

    #[test]
    fn in_memory_delete_missing_is_ok() {
        let store = InMemoryStore::new();
        store.delete(Uuid::new_v4()).unwrap();
    }
}
