//! `HashiCorp` Vault KV v2 secret-engine provider.
//!
//! Implements [`super::VaultProvider`] by issuing an
//! `HTTP GET ${address}/v1/${path}` against the configured Vault
//! address and pulling the requested field out of the response's
//! `data.data` map.
//!
//! # Scope
//!
//! * **KV v2 only.** That's what 99% of enterprise installations use;
//!   adding KV v1 / database / transit engines is a v2.x follow-up.
//!   The path the user writes is forwarded verbatim, so a user with a
//!   v1 mount can still write `vault:hashicorp/secret/db/prod#pw` and
//!   the resulting JSON will fail the `data.data` unwrap with a clear
//!   `BadResponse`. That's acceptable; KV v1 needs its own provider.
//! * **Token auth only.** The token is read from an env var named in
//!   [`crate::settings::HashicorpVaultSettings::token_env`]. Approle
//!   / JWT login flows belong in a separate provider.
//! * **One refresh on 403.** Tokens occasionally rotate. We re-read
//!   the env var (the operator may have updated it) and retry once
//!   on a 403. No exponential backoff loop.
//! * **No persistent cache.** The in-flight dedup map collapses
//!   concurrent resolves of the same reference into one HTTP call;
//!   resolved values are not stored. Caching is a separate design
//!   axis (TTL, invalidation, threat model) and out of scope for v1.
//!
//! # Wire shape
//!
//! `GET /v1/secret/data/db/prod` returns
//!
//! ```json
//! {
//!   "data": {
//!     "data": { "password": "s3cret", "username": "alice" },
//!     "metadata": { ... }
//!   }
//! }
//! ```
//!
//! `Reference::field` selects the key inside `data.data`. When `field`
//! is `None` and `data.data` has exactly one entry, that entry's value
//! is used so `vault:hashicorp/secret/data/db/prod` works on a single-
//! field KV. Multi-field paths without an explicit selector return a
//! [`VaultError::BadResponse`].

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use secrecy::SecretString;
use serde::Deserialize;

use super::{Reference, VaultError, VaultProvider};

/// Default per-call timeout for KV v2 reads.
pub const DEFAULT_HASHICORP_TIMEOUT_SECS: u64 = 5;

#[derive(Debug)]
pub struct HashicorpVault {
    name: String,
    address: String,
    token_env: String,
    namespace: Option<String>,
    timeout: Duration,
    client: Client,
}

impl HashicorpVault {
    /// Construct a provider from a settings struct.
    ///
    /// The `name` argument is the *provider name* under which this
    /// instance will be registered in the [`super::VaultRegistry`].
    /// Vault references address providers by name
    /// (`vault:<name>/<path>`), so the registry must know which
    /// label to expose this instance under. The conventional value
    /// is `"hashicorp"`.
    pub fn from_settings(
        name: impl Into<String>,
        settings: &crate::settings::HashicorpVaultSettings,
    ) -> Result<Self, VaultError> {
        let name = name.into();
        let address = settings
            .address
            .clone()
            .ok_or_else(|| VaultError::NotConfigured {
                provider: name.clone(),
                reason: "settings.vault.providers.hashicorp.address is unset".into(),
            })?;
        let token_env = settings
            .token_env
            .clone()
            .ok_or_else(|| VaultError::NotConfigured {
                provider: name.clone(),
                reason: "settings.vault.providers.hashicorp.token_env is unset".into(),
            })?;
        let timeout = Duration::from_secs(
            settings
                .timeout_secs
                .unwrap_or(DEFAULT_HASHICORP_TIMEOUT_SECS),
        );
        // Build a client with a connect-and-request timeout that is
        // slightly *less* than the per-call timeout, so the per-call
        // budget is the upper bound the user sees.
        let client =
            Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| VaultError::NotConfigured {
                    provider: name.clone(),
                    reason: format!("reqwest builder failed: {e}"),
                })?;
        Ok(Self {
            name,
            address: address.trim_end_matches('/').to_owned(),
            token_env,
            namespace: settings.namespace.clone(),
            timeout,
            client,
        })
    }

    fn read_token(&self) -> Result<String, VaultError> {
        let val = std::env::var(&self.token_env).map_err(|_| VaultError::NotConfigured {
            provider: self.name.clone(),
            reason: format!(
                "env var `{}` (settings.vault.providers.hashicorp.token_env) is unset",
                self.token_env
            ),
        })?;
        if val.is_empty() {
            return Err(VaultError::NotConfigured {
                provider: self.name.clone(),
                reason: format!("env var `{}` is empty", self.token_env),
            });
        }
        Ok(val)
    }

    async fn fetch_once(&self, reference: &Reference) -> Result<KvV2Response, VaultError> {
        let token = self.read_token()?;
        let url = format!("{}/v1/{}", self.address, reference.path);
        let mut req = self
            .client
            .get(&url)
            .header("X-Vault-Token", token)
            .header("Accept", "application/json");
        if let Some(ns) = &self.namespace {
            req = req.header("X-Vault-Namespace", ns);
        }
        let send_fut = req.send();
        let response = tokio::time::timeout(self.timeout, send_fut)
            .await
            .map_err(|_| VaultError::Timeout {
                reference: reference.raw.clone(),
                seconds: self.timeout.as_secs(),
            })?
            .map_err(|e| VaultError::from_reqwest(&reference.raw, &e))?;

        let status = response.status();
        if !status.is_success() {
            // Try to capture a short body excerpt for diagnostics
            // without ever logging the resolved secret (vault never
            // echoes secret material in non-2xx bodies — those carry
            // only error metadata).
            let body = response.text().await.unwrap_or_default();
            return Err(VaultError::from_http_status(&reference.raw, status, &body)
                .unwrap_or_else(|| VaultError::BadResponse {
                    reference: reference.raw.clone(),
                    reason: format!("HTTP {status}"),
                }));
        }

        let body_text = tokio::time::timeout(self.timeout, response.text())
            .await
            .map_err(|_| VaultError::Timeout {
                reference: reference.raw.clone(),
                seconds: self.timeout.as_secs(),
            })?
            .map_err(|e| VaultError::BadResponse {
                reference: reference.raw.clone(),
                reason: format!("read body: {e}"),
            })?;

        serde_json::from_str::<KvV2Response>(&body_text).map_err(|e| VaultError::BadResponse {
            reference: reference.raw.clone(),
            reason: format!("parse KV v2 JSON: {e}"),
        })
    }
}

impl VaultProvider for HashicorpVault {
    fn name(&self) -> &str {
        &self.name
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>> {
        Box::pin(async move {
            // First attempt.
            let response = match self.fetch_once(reference).await {
                Ok(r) => r,
                // One retry on Denied — token may have rotated and
                // the operator updated the env var. Anything else
                // bubbles up unchanged.
                Err(VaultError::Denied { .. }) => self.fetch_once(reference).await?,
                Err(e) => return Err(e),
            };

            let map = response.data.data;
            let value = match &reference.field {
                Some(field) => map
                    .get(field)
                    .cloned()
                    .ok_or_else(|| VaultError::BadResponse {
                        reference: reference.raw.clone(),
                        reason: format!("KV v2 path missing field `{field}`"),
                    })?,
                None => {
                    if map.len() == 1 {
                        // Convenience: unambiguous single-field path.
                        map.into_values().next().unwrap_or_default()
                    } else {
                        return Err(VaultError::BadResponse {
                            reference: reference.raw.clone(),
                            reason: format!(
                                "KV v2 path returns {} fields; specify one with `#field`",
                                map.len()
                            ),
                        });
                    }
                }
            };
            Ok(Arc::new(SecretString::new(value.into_boxed_str())))
        })
    }
}

#[derive(Debug, Deserialize)]
struct KvV2Response {
    data: KvV2Inner,
}

#[derive(Debug, Deserialize)]
struct KvV2Inner {
    data: std::collections::BTreeMap<String, String>,
}
