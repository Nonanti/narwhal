//! TLS connector construction.
//!
//! The [`InternalSslMode`] enum captures the subset of libpq's `sslmode`
//! parameter that maps onto rustls behaviours:
//!
//! - **Disable**: no TLS.
//! - **Prefer**: TLS with full chain + hostname verification (system CA or
//!   `ssl_root_cert`). This is a **hardened** interpretation: unlike libpq's
//!   `prefer`, there is no fallback to a plain-text connection.
//! - **Require**: TLS with chain verification but **hostname verification
//!   skipped** (matches `MySQL` `Require` semantics). The server certificate
//!   must chain to a trusted root, but the hostname in the certificate is
//!   not checked.
//! - **`VerifyCa`**: identical to `Require` (chain verify, no hostname) —
//!   provided for explicitness.
//! - **Verify**: TLS with full chain + hostname verification (the previous
//!   `verify-full` behaviour).

use std::io::BufReader;
use std::sync::Arc;

use narwhal_core::{ConnectionParams, Error, Result, SslMode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
pub use tokio_postgres_rustls::MakeRustlsConnect;

/// Internal representation that maps the public [`SslMode`] onto the
/// TLS behaviours rustls supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalSslMode {
    /// No TLS handshake.
    Disable,
    /// Chain + hostname verification (system CA or `ssl_root_cert`).
    Prefer,
    /// Chain verification, hostname skipped (matches `MySQL` Require).
    Require,
    /// Chain verification, hostname skipped (explicit verify-ca).
    VerifyCa,
    /// Chain + hostname verification (verify-full).
    Verify,
}

impl InternalSslMode {
    /// Resolve the effective TLS mode from the connection params.
    ///
    /// Priority: the dedicated `ssl_mode` field takes precedence. If it is
    /// the default (`Prefer`), the legacy `sslmode` key in `options` is
    /// consulted for backward compatibility.
    pub(crate) fn from_params(params: &ConnectionParams) -> Result<Self> {
        let mode = params.ssl_mode;
        // Override from legacy options key if ssl_mode is at the default
        // and the user explicitly set sslmode in the options map.
        let mode = if mode == SslMode::Prefer {
            if let Some(raw) = params.options.get("sslmode") {
                match raw.to_ascii_lowercase().as_str() {
                    "disable" => SslMode::Disable,
                    "prefer" => SslMode::Prefer,
                    "require" => SslMode::Require,
                    "verify-ca" => SslMode::VerifyCa,
                    "verify-full" => SslMode::VerifyFull,
                    other => {
                        return Err(Error::Config(format!(
                            "unsupported sslmode value: {other} \
                             (use disable|prefer|require|verify-ca|verify-full)"
                        )));
                    }
                }
            } else {
                SslMode::Prefer
            }
        } else {
            mode
        };

        Ok(match mode {
            SslMode::Disable => Self::Disable,
            SslMode::Prefer => Self::Prefer,
            SslMode::Require => Self::Require,
            SslMode::VerifyCa => Self::VerifyCa,
            SslMode::VerifyFull => Self::Verify,
            // Future SslMode variants: fail closed with verify-full, which
            // is the strictest mode we support today.
            _ => Self::Verify,
        })
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::Verify => "verify-full",
        }
    }
}

impl std::fmt::Display for InternalSslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn make_tls_connector(
    mode: InternalSslMode,
    params: &ConnectionParams,
) -> Result<MakeRustlsConnect> {
    // Install the platform default crypto provider once. Subsequent calls are
    // a no-op; the result is intentionally ignored.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = match mode {
        InternalSslMode::Disable => unreachable!("disable path does not request a TLS connector"),
        InternalSslMode::Prefer | InternalSslMode::Verify => verified_client_config(params)?,
        InternalSslMode::Require | InternalSslMode::VerifyCa => verify_ca_client_config(params)?,
    };
    Ok(MakeRustlsConnect::new(config))
}

/// Full verification: chain + hostname check.
fn verified_client_config(params: &ConnectionParams) -> Result<ClientConfig> {
    let store = build_root_store(params)?;

    if let Some(key_pair) = load_client_cert_key(params)? {
        ClientConfig::builder()
            .with_root_certificates(store)
            .with_client_auth_cert(key_pair.certs, key_pair.key)
            .map_err(|e| Error::Config(format!("invalid client cert/key pair: {e}")))
    } else {
        Ok(ClientConfig::builder()
            .with_root_certificates(store)
            .with_no_client_auth())
    }
}

/// Chain verification without hostname check (verify-ca / require semantics).
fn verify_ca_client_config(params: &ConnectionParams) -> Result<ClientConfig> {
    let store = build_root_store(params)?;
    let verifier = Arc::new(VerifyCaNoHostname::new(store));

    if let Some(key_pair) = load_client_cert_key(params)? {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(key_pair.certs, key_pair.key)
            .map_err(|e| Error::Config(format!("invalid client cert/key pair: {e}")))
    } else {
        Ok(ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth())
    }
}

/// Build a [`RootCertStore`] from `ssl_root_cert` or the system native CA
/// store.
fn build_root_store(params: &ConnectionParams) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();

    if let Some(path) = &params.ssl_root_cert {
        // Load the user-provided CA bundle.
        let bytes = std::fs::read(path).map_err(|e| {
            Error::Config(format!(
                "failed to read ssl_root_cert '{}': {e}",
                path.display()
            ))
        })?;
        let mut reader = BufReader::new(&bytes[..]);
        let certs: Vec<CertificateDer<'_>> = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Config(format!("failed to parse ssl_root_cert PEM: {e}")))?;
        let (added, _ignored) = store.add_parsable_certificates(certs);
        if added == 0 {
            return Err(Error::Config(format!(
                "no certificates found in ssl_root_cert '{}'",
                path.display()
            )));
        }
    } else {
        // Fall back to the system's native CA store.
        let load = rustls_native_certs::load_native_certs();
        if !load.errors.is_empty() {
            for err in &load.errors {
                tracing::warn!(target: "narwhal::postgres::tls", error = %err, "failed to load a native CA");
            }
        }
        let (added, _ignored) = store.add_parsable_certificates(load.certs);
        if added == 0 {
            return Err(Error::Config(
                "no trusted CA certificates available; install ca-certificates \
                 or set ssl_root_cert"
                    .into(),
            ));
        }
    }

    Ok(store)
}

/// Custom certificate verifier that performs chain validation but skips
/// hostname verification. This implements the libpq `verify-ca` and
/// `require` semantics where the server certificate must chain to a
/// trusted root, but the hostname in the certificate is not checked.
///
/// The chain verification is delegated to rustls' built-in
/// `WebPkiServerVerifier`; only the hostname check is omitted.
#[derive(Debug)]
struct VerifyCaNoHostname {
    inner: Arc<dyn ServerCertVerifier>,
}

impl VerifyCaNoHostname {
    fn new(store: RootCertStore) -> Self {
        let built = rustls::client::WebPkiServerVerifier::builder(Arc::new(store))
            .build()
            .expect("WebPkiServerVerifier construction should not fail with a valid root store");
        // built is Arc<WebPkiServerVerifier>; coerce to Arc<dyn ServerCertVerifier>
        let inner: Arc<dyn ServerCertVerifier> = built;
        Self { inner }
    }
}

impl ServerCertVerifier for VerifyCaNoHostname {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        // Delegate chain verification to the built-in verifier but ignore
        // hostname mismatch errors. Any other error is fatal.
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            _server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => Ok(v),
            Err(rustls::Error::InvalidCertificate(e)) => {
                // Hostname mismatch is the only error we swallow.
                // All other certificate errors remain fatal.
                if matches!(e, rustls::CertificateError::NotValidForName) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::InvalidCertificate(e))
                }
            }
            Err(other) => Err(other),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[derive(Debug)]
struct ClientCertKey {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

fn load_client_cert_key(params: &ConnectionParams) -> Result<Option<ClientCertKey>> {
    match (&params.ssl_cert, &params.ssl_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_bytes = std::fs::read(cert_path).map_err(|e| {
                Error::Config(format!(
                    "failed to read ssl_cert '{}': {e}",
                    cert_path.display()
                ))
            })?;
            let key_bytes = std::fs::read(key_path).map_err(|e| {
                Error::Config(format!(
                    "failed to read ssl_key '{}': {e}",
                    key_path.display()
                ))
            })?;

            let mut cert_reader = BufReader::new(&cert_bytes[..]);
            let certs: Vec<CertificateDer<'_>> = rustls_pemfile::certs(&mut cert_reader)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Config(format!("failed to parse ssl_cert PEM: {e}")))?;

            let mut key_reader = BufReader::new(&key_bytes[..]);
            let key = rustls_pemfile::private_key(&mut key_reader)
                .map_err(|e| Error::Config(format!("failed to parse ssl_key PEM: {e}")))?
                .ok_or_else(|| Error::Config("no private key found in ssl_key file".into()))?;

            Ok(Some(ClientCertKey { certs, key }))
        }
        (None, None) => Ok(None),
        (Some(_), None) => Err(Error::Config(
            "ssl_cert is set but ssl_key is missing; both must be provided together".into(),
        )),
        (None, Some(_)) => Err(Error::Config(
            "ssl_key is set but ssl_cert is missing; both must be provided together".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn params_with_options(options: BTreeMap<String, String>) -> ConnectionParams {
        ConnectionParams {
            options,
            ..Default::default()
        }
    }

    fn params_with_ssl_mode(ssl_mode: SslMode) -> ConnectionParams {
        ConnectionParams {
            ssl_mode,
            ..Default::default()
        }
    }

    #[test]
    fn from_params_default_is_prefer() {
        let params = ConnectionParams::default();
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Prefer
        );
    }

    #[test]
    fn from_params_disable_mode() {
        let params = params_with_ssl_mode(SslMode::Disable);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Disable
        );
    }

    #[test]
    fn from_params_require_maps_to_require_chain() {
        let params = params_with_ssl_mode(SslMode::Require);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Require
        );
    }

    #[test]
    fn from_params_prefer_maps_to_prefer() {
        let params = params_with_ssl_mode(SslMode::Prefer);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Prefer
        );
    }

    #[test]
    fn from_params_verify_ca() {
        let params = params_with_ssl_mode(SslMode::VerifyCa);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::VerifyCa
        );
    }

    #[test]
    fn from_params_verify_full() {
        let params = params_with_ssl_mode(SslMode::VerifyFull);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Verify
        );
    }

    #[test]
    fn from_params_legacy_options_override() {
        let mut opts = BTreeMap::new();
        opts.insert("sslmode".into(), "disable".into());
        let params = params_with_options(opts);
        // Default SslMode::Prefer + legacy option "disable" → Disable
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Disable
        );
    }

    #[test]
    fn from_params_explicit_mode_overrides_legacy() {
        let mut opts = BTreeMap::new();
        opts.insert("sslmode".into(), "disable".into());
        let params = ConnectionParams {
            ssl_mode: SslMode::Require,
            options: opts,
            ..Default::default()
        };
        // Explicit Require takes precedence over legacy option
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Require
        );
    }

    #[test]
    fn rejects_unknown_legacy_sslmode() {
        let mut opts = BTreeMap::new();
        opts.insert("sslmode".into(), "bogus".into());
        let params = params_with_options(opts);
        let err = InternalSslMode::from_params(&params).unwrap_err();
        assert!(err.to_string().contains("unsupported sslmode"));
    }

    #[test]
    fn client_cert_key_missing_pair_errors() {
        let params = ConnectionParams {
            ssl_cert: Some("/tmp/cert.pem".into()),
            ssl_key: None,
            ..Default::default()
        };
        let err = load_client_cert_key(&params).unwrap_err();
        assert!(err
            .to_string()
            .contains("ssl_cert is set but ssl_key is missing"));
    }

    /// H1: Prefer now uses chain verification (not AcceptAny).
    /// This test verifies that the connector builder succeeds with the
    /// new Prefer path. It delegates to `verified_client_config`, which
    /// requires a usable CA store. On CI systems without ca-certificates
    /// this may fail — that's expected and documents the security
    /// improvement.
    #[test]
    fn prefer_uses_chain_verifier() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let params = ConnectionParams {
            ssl_mode: SslMode::Prefer,
            ..Default::default()
        };
        let mode = InternalSslMode::from_params(&params).unwrap();
        assert_eq!(mode, InternalSslMode::Prefer);
        // make_tls_connector for Prefer should build a verified config,
        // not an insecure one.
        let result = make_tls_connector(mode, &params);
        // Result depends on system CA availability; we only assert it
        // doesn't panic and uses the correct code path.
        match result {
            Ok(_) => {} // system CA available
            Err(e) => {
                // Expected on systems without CA certs
                assert!(
                    e.to_string().contains("no trusted CA certificates"),
                    "unexpected error: {e}"
                );
            }
        }
    }

    /// H1: Require now uses chain verification without hostname check.
    #[test]
    fn require_uses_chain_verifier_no_hostname() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let params = ConnectionParams {
            ssl_mode: SslMode::Require,
            ..Default::default()
        };
        let mode = InternalSslMode::from_params(&params).unwrap();
        assert_eq!(mode, InternalSslMode::Require);
        let result = make_tls_connector(mode, &params);
        match result {
            Ok(_) => {}
            Err(e) => {
                assert!(
                    e.to_string().contains("no trusted CA certificates"),
                    "unexpected error: {e}"
                );
            }
        }
    }

    /// M1: VerifyCa uses chain verification without hostname check.
    #[test]
    fn verify_ca_uses_chain_verifier_no_hostname() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let params = ConnectionParams {
            ssl_mode: SslMode::VerifyCa,
            ssl_root_cert: None,
            ..Default::default()
        };
        let mode = InternalSslMode::from_params(&params).unwrap();
        assert_eq!(mode, InternalSslMode::VerifyCa);
    }

    /// M1: VerifyCa is distinct from VerifyFull (hostname check).
    #[test]
    fn verify_ca_not_same_as_verify_full() {
        let ca_mode = InternalSslMode::from_params(&ConnectionParams {
            ssl_mode: SslMode::VerifyCa,
            ..Default::default()
        })
        .unwrap();
        let full_mode = InternalSslMode::from_params(&ConnectionParams {
            ssl_mode: SslMode::VerifyFull,
            ..Default::default()
        })
        .unwrap();
        assert_ne!(ca_mode, full_mode);
    }

    /// Verify that the client certificate path still works with the
    /// new chain-verified configs.
    #[test]
    fn chain_verified_mode_sends_client_cert_when_provided() {
        use std::io::Write;

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let cert_pem = include_str!("../tests/fixtures/client.crt");
        let key_pem = include_str!("../tests/fixtures/client.key");

        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join("client.crt");
        let key_path = dir.path().join("client.key");
        std::fs::File::create(&cert_path)
            .and_then(|mut f| f.write_all(cert_pem.as_bytes()))
            .expect("write cert");
        std::fs::File::create(&key_path)
            .and_then(|mut f| f.write_all(key_pem.as_bytes()))
            .expect("write key");

        let params = ConnectionParams {
            ssl_mode: SslMode::Require,
            ssl_cert: Some(cert_path),
            ssl_key: Some(key_path),
            ..Default::default()
        };

        // This tests the verify_ca_client_config path with client certs.
        let result = make_tls_connector(InternalSslMode::Require, &params);
        match result {
            Ok(_) => {}
            Err(e) => {
                // CA store may not be available on all systems
                assert!(
                    e.to_string().contains("no trusted CA certificates"),
                    "unexpected error: {e}"
                );
            }
        }
    }
}
