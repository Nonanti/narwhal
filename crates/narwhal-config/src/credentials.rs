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
