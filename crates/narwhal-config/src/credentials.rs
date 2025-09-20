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
    fn from(e: keyring::Error) -> Self {
        match e {
            keyring::Error::NoEntry => CredentialError::NotFound,
            other => CredentialError::Keyring(other.to_string()),
        }
    }
}

/// Trait so tests / SSH-less environments can swap in an in-memory store.
pub trait CredentialStore: Send + Sync {
    fn get(&self, connection_id: Uuid) -> Result<Option<String>, CredentialError>;
    fn set(&self, connection_id: Uuid, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError>;
}

const SERVICE: &str = "narwhal";

pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(connection_id: Uuid) -> Result<keyring::Entry, CredentialError> {
        let user = connection_id.to_string();
        keyring::Entry::new(SERVICE, &user).map_err(Into::into)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for KeyringStore {
    fn get(&self, connection_id: Uuid) -> Result<Option<String>, CredentialError> {
        match Self::entry(connection_id)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, connection_id: Uuid, secret: &str) -> Result<(), CredentialError> {
        Self::entry(connection_id)?.set_password(secret)?;
        Ok(())
    }

    fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        match Self::entry(connection_id)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
