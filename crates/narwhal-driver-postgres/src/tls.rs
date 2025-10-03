//! TLS connector construction.
//!
//! The [`SslMode`] enum captures the subset of libpq's `sslmode` parameter
//! that maps cleanly onto rustls. `verify-ca` and `verify-full` are treated
//! identically because rustls always performs full chain validation through
//! [`WebPkiServerVerifier`]; selecting `verify-ca` only skips the hostname
//! check in libpq, which is a footgun we choose not to replicate.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use narwhal_core::{Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_postgres_rustls::MakeRustlsConnect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SslMode {
    Disable,
    Require,
    Verify,
}

impl SslMode {
    pub(crate) fn from_options(options: &BTreeMap<String, String>) -> Result<Self> {
        let Some(raw) = options.get("sslmode") else {
            return Ok(Self::Disable);
        };
        match raw.to_ascii_lowercase().as_str() {
            "disable" => Ok(Self::Disable),
            "require" | "prefer" => Ok(Self::Require),
            "verify-ca" | "verify-full" => Ok(Self::Verify),
            other => Err(Error::Config(format!(
                "unsupported sslmode value: {other} (use disable|require|verify-ca|verify-full)"
            ))),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Require => "require",
            Self::Verify => "verify-full",
        }
    }
}

impl fmt::Display for SslMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) fn make_tls_connector(mode: SslMode) -> Result<MakeRustlsConnect> {
    // Install the platform default crypto provider once. Subsequent calls are
    // a no-op; the result is intentionally ignored.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = match mode {
        SslMode::Disable => unreachable!("disable path does not request a TLS connector"),
        SslMode::Require => insecure_client_config(),
        SslMode::Verify => verified_client_config()?,
    };
    Ok(MakeRustlsConnect::new(config))
}

fn verified_client_config() -> Result<ClientConfig> {
    let mut store = RootCertStore::empty();
    let load = rustls_native_certs::load_native_certs();
    if !load.errors.is_empty() {
        for err in &load.errors {
            tracing::warn!(target: "narwhal::postgres::tls", error = %err, "failed to load a native CA");
        }
    }
    let (added, _ignored) = store.add_parsable_certificates(load.certs);
    if added == 0 {
        return Err(Error::Config(
            "no trusted CA certificates available; install ca-certificates or use sslmode=require"
                .into(),
        ));
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth())
}

fn insecure_client_config() -> ClientConfig {
    ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth()
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

    #[test]
    fn parses_known_modes() {
        let mut opts = BTreeMap::new();
        assert_eq!(SslMode::from_options(&opts).unwrap(), SslMode::Disable);

        opts.insert("sslmode".into(), "disable".into());
        assert_eq!(SslMode::from_options(&opts).unwrap(), SslMode::Disable);

        opts.insert("sslmode".into(), "Require".into());
        assert_eq!(SslMode::from_options(&opts).unwrap(), SslMode::Require);

        opts.insert("sslmode".into(), "verify-ca".into());
        assert_eq!(SslMode::from_options(&opts).unwrap(), SslMode::Verify);

        opts.insert("sslmode".into(), "verify-full".into());
        assert_eq!(SslMode::from_options(&opts).unwrap(), SslMode::Verify);
    }

    #[test]
    fn rejects_unknown_mode() {
        let mut opts = BTreeMap::new();
        opts.insert("sslmode".into(), "bogus".into());
        let err = SslMode::from_options(&opts).unwrap_err();
        assert!(err.to_string().contains("unsupported sslmode"));
    }
}
