use std::collections::HashMap;
use std::future::Future;

use narwhal_core::{ConnectionConfig, ConnectionParams};
use parking_lot::Mutex;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::vault::{Reference, VaultError, VaultRegistry};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredentialError {
    #[error("credential not found")]
    NotFound,
    #[error("keyring error: {0}")]
    Keyring(String),
    /// Vault provider error. Wraps a [`VaultError`] so callers can
    /// inspect the specific failure class (unreachable, denied,
    /// not-found) when deciding whether to retry..
    #[error("vault: {0}")]
    Vault(#[from] VaultError),
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
///
/// # Trait shape
///
/// This trait uses **native `async fn` in trait** (RPITIT) — every
/// `async fn` desugars to `-> impl Future + Send`. Because RPITIT is
/// **not** dyn-compatible, callers that need a trait object should use
/// [`DynCredentialStore`] instead: it boxes the returned future, costing an
/// allocation per call but enabling `Box<dyn DynCredentialStore>` /
/// `Arc<dyn DynCredentialStore>` sites. A blanket
/// `impl<T: CredentialStore> DynCredentialStore for T` is provided, so any
/// type that implements `CredentialStore` automatically implements
/// `DynCredentialStore`.
pub trait CredentialStore: Send + Sync {
    fn get(
        &self,
        connection_id: Uuid,
    ) -> impl Future<Output = Result<Option<SecretString>, CredentialError>> + Send;

    fn set(
        &self,
        connection_id: Uuid,
        secret: SecretString,
    ) -> impl Future<Output = Result<(), CredentialError>> + Send;

    fn delete(
        &self,
        connection_id: Uuid,
    ) -> impl Future<Output = Result<(), CredentialError>> + Send;
}

/// Dyn-safe sibling of [`CredentialStore`].
///
/// Native `async fn` in trait isn't dyn-compatible — the returned
/// future has an existential type that can't fit in a vtable slot.
/// `DynCredentialStore` is the boxing wrapper: every async method returns
/// `Pin<Box<dyn Future + Send + '_>>`, which **is** vtable-friendly.
///
/// A blanket `impl<T: CredentialStore> DynCredentialStore for T` means any
/// `CredentialStore` automatically satisfies `DynCredentialStore`. Callers that
/// need a trait object — the binary entry point, the MCP context, the
/// app shell — use `Arc<dyn DynCredentialStore>` and pay the classic
/// `Box<dyn Future>` alloc per call. Callers with a concrete type call
/// [`CredentialStore`] directly and avoid the alloc.
///
/// Trait definitions intentionally keep explicit `'a` lifetimes on the
/// dyn-safe methods: every borrowed parameter shares the same lifetime
/// as the returned `BoxFuture`, which elision cannot express.
#[allow(clippy::needless_lifetimes, clippy::elidable_lifetime_names)]
pub trait DynCredentialStore: Send + Sync {
    fn get<'a>(
        &'a self,
        connection_id: Uuid,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<SecretString>, CredentialError>>
                + Send
                + 'a,
        >,
    >;

    fn set<'a>(
        &'a self,
        connection_id: Uuid,
        secret: SecretString,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CredentialError>> + Send + 'a>>;

    fn delete<'a>(
        &'a self,
        connection_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CredentialError>> + Send + 'a>>;
}

impl<T> DynCredentialStore for T
where
    T: CredentialStore + 'static,
{
    fn get<'a>(
        &'a self,
        connection_id: Uuid,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<SecretString>, CredentialError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(<Self as CredentialStore>::get(self, connection_id))
    }

    fn set<'a>(
        &'a self,
        connection_id: Uuid,
        secret: SecretString,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CredentialError>> + Send + 'a>>
    {
        Box::pin(<Self as CredentialStore>::set(self, connection_id, secret))
    }

    fn delete<'a>(
        &'a self,
        connection_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), CredentialError>> + Send + 'a>>
    {
        Box::pin(<Self as CredentialStore>::delete(self, connection_id))
    }
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
        tokio::task::spawn_blocking(move || {
            let res = Self::set_blocking(id, &plain);
            plain.zeroize();
            res
        })
        .await
        .map_err(|e| CredentialError::Keyring(format!("spawn_blocking join error: {e}")))?
    }

    async fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        let id = connection_id;
        tokio::task::spawn_blocking(move || Self::delete_blocking(id))
            .await
            .map_err(|e| CredentialError::Keyring(format!("spawn_blocking join error: {e}")))?
    }
}

/// Resolve a connection password through the full v2 stack.
///
/// Layered resolution, highest priority first:
///
/// 1. **Vault reference**. If [`ConnectionParams::password`] is
/// `Some(s)` and `s` parses as a `vault:` / `1password:`
/// reference, the configured [`VaultRegistry`] resolves it.
/// The keyring is *not* consulted in this branch: the user has
/// opted in to vault storage and silently falling back to a
/// stale keyring entry would defeat the security intent.
/// 2. **Inline / `${env:VAR}` literal**. If `password` is
/// `Some(s)` and is not a reference, `s` is used verbatim
/// (env-interpolation already ran at file load).
/// 3. **Keyring**. If `password` is `None`, the credential store
/// is consulted by `connection.id`.
/// 4. **`~/.pgpass` / env fallback**. Last resort for users who
/// have a libpq-shaped workflow already configured.
///
/// The `vault` argument is `Option<&VaultRegistry>` so callers that
/// have no providers configured can pass `None` and incur zero
/// allocation. When a vault reference is present in the config but
/// `vault` is `None`, the function returns
/// [`CredentialError::Vault`] wrapping [`VaultError::NotConfigured`]
/// — silent fallback to a stale keyring entry would be a security
/// regression.
///
/// Callers that previously used
/// `narwhal_config::resolve_fallback_password` should switch to
/// this function and pass any vault registry they hold. The legacy
/// helper is still exported for the test suite and for downstream
/// crates mid-migration.
pub async fn resolve_password(
    config: &ConnectionConfig,
    vault: Option<&VaultRegistry>,
    keyring: Option<&dyn DynCredentialStore>,
) -> Result<Option<SecretString>, CredentialError> {
    if let Some(raw) = config.params.password.as_deref() {
        let trimmed = raw.trim();
        match Reference::try_parse(trimmed) {
            Ok(Some(reference)) => {
                let registry = vault.ok_or_else(|| VaultError::NotConfigured {
                    provider: reference.provider.clone(),
                    reason: "connection `password` is a vault reference but no \
                             VaultRegistry was constructed (settings.vault.providers \
                             is empty)"
                        .into(),
                })?;
                let secret = registry.resolve(&reference).await?;
                // The Arc-wrapped SecretString is collapsed into an
                // owned SecretString at the seam so the caller does
                // not have to thread the Arc through driver code.
                return Ok(Some(SecretString::new(
                    secret.expose_secret().to_owned().into_boxed_str(),
                )));
            }
            Ok(None) => {
                // Inline literal (already env-interpolated). Empty
                // strings short-circuit to None so an accidental
                // `password = ""` does not mask a keyring entry.
                if !raw.is_empty() {
                    return Ok(Some(SecretString::new(raw.to_owned().into_boxed_str())));
                }
            }
            Err(e) => return Err(CredentialError::Vault(e)),
        }
    }

    if let Some(store) = keyring {
        match store.get(config.id).await {
            Ok(Some(secret)) => return Ok(Some(secret)),
            Ok(None) => {}
            Err(error) => {
                // Surface keyring failures at debug level and fall
                // through to the pgpass/env path — same behaviour the
                // v1.x resolver had.
                tracing::debug!(
                    target: "narwhal::credentials",
                    connection = %config.name,
                    %error,
                    "keyring lookup failed; falling through to pgpass/env",
                );
            }
        }
    }

    Ok(resolve_legacy_fallback(&config.driver, &config.params)
        .map(|s| SecretString::new(s.into_boxed_str())))
}

/// Legacy `~/.pgpass` / env-var fallback, exposed under its own
/// name so the new orchestrator can reuse it without re-importing
/// the pgpass module across crate boundaries.
fn resolve_legacy_fallback(driver: &str, params: &ConnectionParams) -> Option<String> {
    crate::pgpass::resolve_password(driver, params)
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

impl CredentialStore for InMemoryStore {
    async fn get(&self, connection_id: Uuid) -> Result<Option<SecretString>, CredentialError> {
        let guard = self.secrets.lock();
        Ok(guard.get(&connection_id).cloned())
    }

    async fn set(&self, connection_id: Uuid, secret: SecretString) -> Result<(), CredentialError> {
        let mut guard = self.secrets.lock();
        guard.insert(connection_id, secret);
        Ok(())
    }

    async fn delete(&self, connection_id: Uuid) -> Result<(), CredentialError> {
        let mut guard = self.secrets.lock();
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
        assert!(CredentialStore::get(&store, id).await.unwrap().is_none());
        CredentialStore::set(&store, id, SecretString::new("s3cret".into()))
            .await
            .unwrap();
        assert_eq!(
            CredentialStore::get(&store, id)
                .await
                .unwrap()
                .as_ref()
                .map(|s| s.expose_secret() as &str),
            Some("s3cret")
        );
        CredentialStore::delete(&store, id).await.unwrap();
        assert!(CredentialStore::get(&store, id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn in_memory_delete_missing_is_ok() {
        let store = InMemoryStore::new();
        CredentialStore::delete(&store, Uuid::new_v4())
            .await
            .unwrap();
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
        let store: Arc<dyn DynCredentialStore> = Arc::new(InMemoryStore::new());
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
