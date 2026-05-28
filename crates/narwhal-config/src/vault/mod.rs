//! Secret-vault providers — (v2.0).
//!
//! Connection passwords in `connections.toml` can now express:
//!
//! ```toml
//! password = "vault:hashicorp/secret/data/db/prod#password"
//! password = "1password:op://Vault/Postgres/password"
//! ```
//!
//! At connect time, [`crate::credentials::resolve_password`] dispatches
//! references prefixed with `vault:` / `1password:` to this module's
//! registry. The legacy resolution (inline literal → `${env:VAR}` →
//! keyring → `~/.pgpass`) keeps working for users who do not adopt
//! vault storage — the new layer is *purely additive*.
//!
//! # Trait shape
//!
//! [`VaultProvider`] is dyn-safe by construction: the single
//! `resolve` method returns a `BoxFuture` rather than an
//! `impl Future`. The decision diverges from the core
//! `Connection`/`DatabaseDriver` traits (which use the RPITIT +
//! `Dyn*` sibling pattern documented in `docs/dev/async-trait-style.md`)
//! because:
//!
//! 1. Vault providers are *registered into a `HashMap<String, Arc<dyn
//! VaultProvider>>`* — there is no zero-cost call site to optimise
//! for. The dispatch is always through a trait object.
//! 2. There's exactly one async method on the trait. The sibling-and-
//! blanket-impl machinery doubles the surface for no gain.
//! 3. `Arc<SecretString>` is the natural return shape because the
//! in-flight dedup broadcast must hand the same value to every
//! concurrent waiter, and `SecretString` is not itself `Clone`.
//!
//! # Registry
//!
//! [`VaultRegistry`] holds zero or more named providers and contains
//! the in-flight de-duplication layer. The contract is:
//!
//! * Two `resolve` calls for the *same reference* concurrently
//! coalesce into one provider call. The brief's acceptance
//! criterion ("concurrent resolves of the same reference cause one
//! HTTP call, not two") is satisfied by a `broadcast::channel` per
//! in-flight reference.
//! * Cancellation of a waiter does not cancel the in-flight leader.
//! The leader continues to completion so the other waiters get a
//! result; the cancelled task drops its receiver and the broadcast
//! send still succeeds (broadcast tolerates zero receivers).
//!
//! # Logging
//!
//! Provider implementations log at `debug` with the *reference* and
//! the *status class*, never the resolved secret. Errors implement
//! `Display` with the reference baked into the message so an audit
//! log line ties back to the offending config block.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use tokio::sync::broadcast;

pub mod error;
pub mod hashicorp;
pub mod onepassword;
pub mod resolver;

pub use error::VaultError;
pub use hashicorp::{DEFAULT_HASHICORP_TIMEOUT_SECS, HashicorpVault};
pub use onepassword::{DEFAULT_OP_TIMEOUT_SECS, OnepasswordCli};
pub use resolver::Reference;

/// One provider — `hashicorp`, `1password`, or a future addition.
///
/// Implementors are `Send + Sync` so the registry can be shared
/// across the async runtime via `Arc`. The trait is *dyn-safe* on
/// purpose: see the module docs.
pub trait VaultProvider: Send + Sync + std::fmt::Debug {
    /// Lookup label registered under in the [`VaultRegistry`].
    fn name(&self) -> &str;

    /// Resolve `reference` to its secret material.
    ///
    /// The return type is `Arc<SecretString>` so the registry can
    /// hand the *same* `Arc` to every concurrent waiter for the
    /// same reference. Callers that need an owned `SecretString`
    /// can clone the inner via
    /// `SecretString::new(arc.expose_secret().to_owned().into_boxed_str())`
    /// — but the resolver path in [`crate::credentials`] already
    /// performs that copy at the seam, so application code never
    /// has to touch `expose_secret` itself.
    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>>;
}

/// Collection of named providers + in-flight de-duplication.
///
/// Build with [`VaultRegistry::from_settings`] to derive providers
/// from the v2 `settings.vault` section, or with [`VaultRegistry::empty`]
/// for tests / no-vault deployments. [`VaultRegistry::with_provider`]
/// appends additional providers (used by tests that need a stub).
///
/// Cloning the registry is cheap — every provider is an `Arc<dyn …>`
/// and the in-flight map is shared via `Arc`. Stick it on `AppCore`
/// and clone into per-request tasks freely.
#[derive(Clone, Debug, Default)]
pub struct VaultRegistry {
    providers: HashMap<String, Arc<dyn VaultProvider>>,
    in_flight: Arc<InFlight>,
}

/// Per-reference broadcast channel map. Used to coalesce concurrent
/// resolves of the same reference into a single provider call.
#[derive(Debug, Default)]
struct InFlight {
    map: Mutex<HashMap<String, broadcast::Sender<Outcome>>>,
}

type Outcome = Result<Arc<SecretString>, VaultError>;

/// Channel capacity for an in-flight broadcast.
///
/// The leader sends exactly one item and we never have more
/// receivers than concurrent resolvers for a single reference;
/// 16 is enough headroom that the realistic "many sessions opening
/// at app start" scenario doesn't even brush the wall.
const BROADCAST_CAPACITY: usize = 16;

impl VaultRegistry {
    /// Empty registry. Useful for tests and for binaries built
    /// without vault support — `resolve` will always return
    /// [`VaultError::UnknownProvider`].
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a registry from the v2 `[vault]` section of
    /// `settings.toml`. Missing provider sub-sections are skipped
    /// silently (a config that only declares `default_provider =
    /// "none"` produces an empty registry).
    ///
    /// Returns an error only when a *declared* provider sub-section
    /// has invalid settings (e.g. `hashicorp.address` unset). A
    /// reference that names an unregistered provider is a runtime
    /// error returned from [`Self::resolve`], not from this
    /// constructor — so adding the provider later (via
    /// [`Self::with_provider`]) lets the same registry serve.
    pub fn from_settings(settings: &crate::settings::VaultSettings) -> Result<Self, VaultError> {
        let mut registry = Self::empty();
        if let Some(hc) = settings.providers.hashicorp.as_ref() {
            // Construction may fail with NotConfigured; surface as the
            // outer Err so the app can choose to log + continue.
            let provider = HashicorpVault::from_settings("hashicorp", hc)?;
            registry = registry.with_provider(Arc::new(provider));
        }
        if let Some(op) = settings.providers.onepassword.as_ref() {
            let provider = OnepasswordCli::from_settings("1password", op)?;
            registry = registry.with_provider(Arc::new(provider));
        }
        Ok(registry)
    }

    /// Register a provider. Returns `self` so the call chains with
    /// [`Self::from_settings`] for tests that need to inject a stub.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn VaultProvider>) -> Self {
        self.providers.insert(provider.name().to_owned(), provider);
        self
    }

    /// Provider names currently registered. Order is unspecified.
    /// Used by `narwhal config validate` to print the configured set
    /// without exposing the providers themselves.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// True when no providers are registered. Inspected by
    /// [`crate::credentials::resolve_password`] so the caller can
    /// distinguish "no vault configured" from "vault configured but
    /// reference not registered".
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Resolve `reference` through the registered provider, with
    /// in-flight de-duplication.
    ///
    /// Concurrent calls with the same `reference.raw` are coalesced
    /// into a single provider call: the first caller becomes the
    /// *leader* and performs the lookup; subsequent callers
    /// subscribe to a `broadcast::Receiver` and wait for the
    /// leader's result. The leader cleans the in-flight slot before
    /// returning, so a third call after the first two have completed
    /// starts a fresh lookup (no result caching at this layer).
    pub async fn resolve(&self, reference: &Reference) -> Outcome {
        let key = reference.raw.clone();

        // Step 1: get-or-create the broadcast channel under the
        // sync mutex. Holding the std::sync::Mutex across an await
        // is forbidden; we drop the guard before the async work.
        let role = {
            let mut map = self
                .in_flight
                .map
                .lock()
                .map_err(|e| VaultError::BadResponse {
                    reference: key.clone(),
                    reason: format!("in-flight mutex poisoned: {e}"),
                })?;
            if let Some(tx) = map.get(&key) {
                Role::Follower(tx.subscribe())
            } else {
                let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
                map.insert(key.clone(), tx.clone());
                Role::Leader(tx)
            }
        };

        match role {
            Role::Leader(tx) => {
                let result = match self.providers.get(&reference.provider) {
                    Some(provider) => provider.resolve(reference).await,
                    None => Err(VaultError::UnknownProvider {
                        provider: reference.provider.clone(),
                        reference: reference.raw.clone(),
                    }),
                };
                // Broadcast first so followers wake up, then clean
                // up the map. The send is best-effort: zero
                // subscribers is fine (no receivers will see it).
                let _ = tx.send(result.clone());
                if let Ok(mut map) = self.in_flight.map.lock() {
                    map.remove(&key);
                }
                result
            }
            Role::Follower(mut rx) => match rx.recv().await {
                Ok(outcome) => outcome,
                Err(_) => Err(VaultError::DedupChannelClosed { reference: key }),
            },
        }
    }
}

enum Role {
    Leader(broadcast::Sender<Outcome>),
    Follower(broadcast::Receiver<Outcome>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        HashicorpVaultSettings, OnePasswordVaultSettings, VaultProviderSettings, VaultSettings,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test stub provider that counts how many times it was called.
    /// Used to verify the de-dup contract.
    #[derive(Debug)]
    struct StubProvider {
        name: String,
        calls: Arc<AtomicUsize>,
        secret: String,
        delay: std::time::Duration,
    }

    impl VaultProvider for StubProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn resolve<'a>(
            &'a self,
            _reference: &'a Reference,
        ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(self.delay).await;
                Ok(Arc::new(SecretString::new(
                    self.secret.clone().into_boxed_str(),
                )))
            })
        }
    }

    #[tokio::test]
    async fn unknown_provider_errors() {
        let registry = VaultRegistry::empty();
        let reference = Reference::try_parse("vault:hashicorp/secret/data/x#p")
            .unwrap()
            .unwrap();
        match registry.resolve(&reference).await.unwrap_err() {
            VaultError::UnknownProvider { provider, .. } => {
                assert_eq!(provider, "hashicorp");
            }
            other => panic!("expected UnknownProvider, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn registered_provider_resolves() {
        use secrecy::ExposeSecret;
        let calls = Arc::new(AtomicUsize::new(0));
        let stub = StubProvider {
            name: "hashicorp".into(),
            calls: calls.clone(),
            secret: "s3cret".into(),
            delay: std::time::Duration::from_millis(1),
        };
        let registry = VaultRegistry::empty().with_provider(Arc::new(stub));
        let reference = Reference::try_parse("vault:hashicorp/secret/data/x#p")
            .unwrap()
            .unwrap();
        let secret = registry.resolve(&reference).await.unwrap();
        assert_eq!(secret.expose_secret(), "s3cret");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_resolves_dedup_into_one_call() {
        use secrecy::ExposeSecret;
        let calls = Arc::new(AtomicUsize::new(0));
        let stub = StubProvider {
            name: "hashicorp".into(),
            calls: calls.clone(),
            secret: "shared".into(),
            delay: std::time::Duration::from_millis(50),
        };
        let registry = VaultRegistry::empty().with_provider(Arc::new(stub));
        let reference = Arc::new(
            Reference::try_parse("vault:hashicorp/secret/data/x#p")
                .unwrap()
                .unwrap(),
        );

        // Fire 8 concurrent resolves.
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let r = reference.clone();
            let reg = registry.clone();
            tasks.push(tokio::spawn(async move { reg.resolve(&r).await }));
        }
        let results = futures::future::join_all(tasks).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "8 concurrent resolves must coalesce into 1 provider call"
        );
        for r in results {
            let secret = r.unwrap().unwrap();
            assert_eq!(secret.expose_secret(), "shared");
        }

        // After completion the in-flight map must be clean.
        assert!(registry.in_flight.map.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn from_settings_handles_empty_providers() {
        let settings = VaultSettings::default();
        let registry = VaultRegistry::from_settings(&settings).unwrap();
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn from_settings_rejects_misconfigured_hashicorp() {
        // address & token_env both None
        let providers = VaultProviderSettings {
            hashicorp: Some(HashicorpVaultSettings::default()),
            ..VaultProviderSettings::default()
        };
        let vault = VaultSettings {
            providers,
            ..VaultSettings::default()
        };
        let err = VaultRegistry::from_settings(&vault).unwrap_err();
        match err {
            VaultError::NotConfigured { provider, .. } => assert_eq!(provider, "hashicorp"),
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn from_settings_accepts_onepassword_default() {
        // OnePassword provider needs only a binary path (defaults to `op`)
        // so an empty settings block is enough to register it.
        let providers = VaultProviderSettings {
            onepassword: Some(OnePasswordVaultSettings::default()),
            ..VaultProviderSettings::default()
        };
        let vault = VaultSettings {
            providers,
            ..VaultSettings::default()
        };
        let registry = VaultRegistry::from_settings(&vault).unwrap();
        assert_eq!(registry.provider_names(), vec!["1password"]);
    }
}
