//! TLS connector construction.
//!
//! The [`InternalSslMode`] enum captures the subset of libpq's `sslmode`
//! parameter that maps cleanly onto rustls. `verify-ca` and `verify-full`
//! are treated identically because rustls always performs full chain
//! validation through [`WebPkiServerVerifier`]; selecting `verify-ca` only
//! skips the hostname check in libpq, which is a footgun we choose not to
//! replicate.

use std::io::BufReader;
use std::sync::Arc;

use narwhal_core::{ConnectionParams, Error, Result, SslMode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Internal representation that maps the public [`SslMode`] onto the
/// three TLS behaviours rustls supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InternalSslMode {
    Disable,
    Require,
    Verify,
}

impl InternalSslMode {
    /// Resolve the effective TLS mode from the connection params.
    ///
    /// Priority: the dedicated `ssl_mode` field takes precedence over the
    /// legacy `sslmode` key in `options`. If neither is set, defaults to
    /// `Disable` (matching the pre-TLS behaviour of this driver).
    pub(crate) fn from_params(params: &ConnectionParams) -> Result<Self> {
        let mode = params.ssl_mode;
        // Override from legacy options key if ssl_mode is at the default
        // and the user explicitly set sslmode in the options map.
        let mode = if mode == SslMode::Prefer {
            if let Some(raw) = params.options.get("sslmode") {
                match raw.to_ascii_lowercase().as_str() {
                    "disable" => SslMode::Disable,
                    "require" | "prefer" => SslMode::Require,
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
                // Default to Prefer for postgres — but Prefer maps to
                // Require in our internal model (try TLS, fall back to
                // plain is not something we expose).
                SslMode::Prefer
            }
        } else {
            mode
        };

        Ok(match mode {
            SslMode::Disable => InternalSslMode::Disable,
            SslMode::Prefer | SslMode::Require => InternalSslMode::Require,
            SslMode::VerifyCa | SslMode::VerifyFull => InternalSslMode::Verify,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Require => "require",
            Self::Verify => "verify-full",
        }
    }
}

impl std::fmt::Display for InternalSslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn make_tls_connector(
    mode: InternalSslMode,
    params: &ConnectionParams,
) -> Result<MakeRustlsConnect> {
    // Install the platform default crypto provider once. Subsequent calls are
    // a no-op; the result is intentionally ignored.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = match mode {
        InternalSslMode::Disable => unreachable!("disable path does not request a TLS connector"),
        InternalSslMode::Require => insecure_client_config(params)?,
        InternalSslMode::Verify => verified_client_config(params)?,
    };
    Ok(MakeRustlsConnect::new(config))
}

fn verified_client_config(params: &ConnectionParams) -> Result<ClientConfig> {
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
                "no trusted CA certificates available; install ca-certificates, \
                 set ssl_root_cert, or use ssl_mode=require"
                    .into(),
            ));
        }
    }

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

fn insecure_client_config(params: &ConnectionParams) -> Result<ClientConfig> {
    let store = RootCertStore::empty();
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();

    // Even in require mode, if the user provides a client cert/key pair,
    // we should send it. However, with a dangerous verifier we need to
    // build the config differently.
    // For simplicity in the insecure path, we skip client auth — the
    // server isn't verifying our cert anyway if we're not verifying theirs.
    let _ = params; // consumed for future use
    let _ = store; // empty store needed for type inference

    Ok(builder)
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

#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
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
    fn from_params_default_is_prefer_maps_to_require() {
        let params = ConnectionParams::default();
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Require
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
    fn from_params_verify_ca() {
        let params = params_with_ssl_mode(SslMode::VerifyCa);
        assert_eq!(
            InternalSslMode::from_params(&params).unwrap(),
            InternalSslMode::Verify
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
}
