//! TLS/SSL configuration tests (serde round-trip + validation).
//!
//! These tests verify the TOML schema and config-level validation only;
//! live TLS handshake tests require a configured TLS-enabled database
//! and are gated behind driver-level feature flags.

use std::path::PathBuf;

use narwhal_config::{ConfigError, ConnectionsFile};
use narwhal_core::SslMode;

/// 1. Round-trip: a TOML with all four TLS fields parses and the
///    deserialised values match.
#[test]
fn config_parses_ssl_fields() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440000"
name = "prod-postgres"
driver = "postgres"

[connection.params]
host = "db.example.com"
port = 5432
database = "analytics"
username = "reader"
ssl_mode = "verify-full"
ssl_root_cert = "/etc/ssl/certs/ca-bundle.crt"
ssl_cert = "/etc/ssl/client-cert.pem"
ssl_key = "/etc/ssl/client-key.pem"
"#;

    let file = ConnectionsFile::load_from_str(toml).expect("parse should succeed");
    assert_eq!(file.connections.len(), 1);

    let conn = &file.connections[0];
    assert_eq!(conn.name, "prod-postgres");
    assert_eq!(conn.driver, "postgres");
    assert_eq!(conn.params.ssl_mode, SslMode::VerifyFull);
    assert_eq!(
        conn.params.ssl_root_cert,
        Some(PathBuf::from("/etc/ssl/certs/ca-bundle.crt"))
    );
    assert_eq!(
        conn.params.ssl_cert,
        Some(PathBuf::from("/etc/ssl/client-cert.pem"))
    );
    assert_eq!(
        conn.params.ssl_key,
        Some(PathBuf::from("/etc/ssl/client-key.pem"))
    );
}

/// 2. verify-ca / verify-full without ssl_root_cert should reject at
///    config load time.
#[test]
fn verify_ca_without_root_cert_rejects() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440001"
name = "bad-tls"
driver = "postgres"

[connection.params]
host = "db.example.com"
database = "analytics"
username = "reader"
ssl_mode = "verify-ca"
"#;

    let result = ConnectionsFile::load_from_str(toml);
    assert!(result.is_err(), "should reject verify-ca without root cert");
    match result.unwrap_err() {
        ConfigError::Validation(msg) => {
            assert!(
                msg.contains("ssl_root_cert"),
                "error should mention ssl_root_cert, got: {msg}"
            );
        }
        other => panic!("expected ConfigError::Validation, got: {other}"),
    }
}

/// 3. sqlite with non-disable ssl_mode should reject at config load time.
#[test]
fn sqlite_with_non_disable_rejects() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440002"
name = "local-sqlite"
driver = "sqlite"

[connection.params]
path = "/tmp/test.db"
ssl_mode = "require"
"#;

    let result = ConnectionsFile::load_from_str(toml);
    assert!(
        result.is_err(),
        "should reject sqlite with ssl_mode=require"
    );
    match result.unwrap_err() {
        ConfigError::Validation(msg) => {
            assert!(
                msg.contains("disable") && msg.contains("sqlite"),
                "error should mention sqlite and disable, got: {msg}"
            );
        }
        other => panic!("expected ConfigError::Validation, got: {other}"),
    }
}

/// 3b. Backwards-compat: a sqlite (or duckdb) connection WITHOUT any
///     ssl_mode field still parses.  The default is `Prefer` but
///     validation tolerates it for file-local drivers — anything else
///     would break every pre-TLS connections.toml that exists in the
///     wild.
#[test]
fn sqlite_without_ssl_mode_still_loads() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440099"
name = "legacy-sqlite"
driver = "sqlite"

[connection.params]
path = "/tmp/legacy.db"
"#;

    let file = ConnectionsFile::load_from_str(toml).expect(
        "sqlite without an explicit ssl_mode must still parse \
         (default Prefer is silently tolerated for file-local drivers)",
    );
    assert_eq!(file.connections.len(), 1);
    assert_eq!(file.connections[0].driver, "sqlite");
}

/// 4a. Partial mTLS: only ssl_cert without ssl_key must be rejected.
#[test]
fn mtls_partial_cert_only_rejected() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440010"
name = "partial-mtls"
driver = "postgres"

[connection.params]
host = "db.example.com"
ssl_mode = "require"
ssl_cert = "/etc/ssl/client-cert.pem"
"#;

    let result = ConnectionsFile::load_from_str(toml);
    assert!(result.is_err(), "should reject ssl_cert without ssl_key");
    match result.unwrap_err() {
        ConfigError::Validation(msg) => {
            assert!(
                msg.contains("ssl_cert") && msg.contains("ssl_key"),
                "error should mention both fields, got: {msg}"
            );
        }
        other => panic!("expected ConfigError::Validation, got: {other}"),
    }
}

/// 4b. Partial mTLS: only ssl_key without ssl_cert must be rejected.
#[test]
fn mtls_partial_key_only_rejected() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440011"
name = "partial-mtls-key"
driver = "clickhouse"

[connection.params]
host = "db.example.com"
ssl_mode = "require"
ssl_key = "/etc/ssl/client-key.pem"
"#;

    let result = ConnectionsFile::load_from_str(toml);
    assert!(result.is_err(), "should reject ssl_key without ssl_cert");
    match result.unwrap_err() {
        ConfigError::Validation(msg) => {
            assert!(
                msg.contains("ssl_cert") && msg.contains("ssl_key"),
                "error should mention both fields, got: {msg}"
            );
        }
        other => panic!("expected ConfigError::Validation, got: {other}"),
    }
}

/// 4c. Both ssl_cert and ssl_key set together passes validation.
#[test]
fn mtls_full_pair_accepted() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440012"
name = "full-mtls"
driver = "postgres"

[connection.params]
host = "db.example.com"
ssl_mode = "verify-full"
ssl_root_cert = "/etc/ssl/certs/ca-bundle.crt"
ssl_cert = "/etc/ssl/client-cert.pem"
ssl_key = "/etc/ssl/client-key.pem"
"#;

    let file = ConnectionsFile::load_from_str(toml).expect("parse should succeed");
    assert_eq!(file.connections.len(), 1);
}

/// 5. TOML missing all SSL fields parses, ssl_mode defaults to Prefer
///    for network drivers.
#[test]
fn default_ssl_mode_prefer_for_network() {
    let toml = r#"
[[connection]]
id = "550e8400-e29b-41d4-a716-446655440003"
name = "plain-mysql"
driver = "mysql"

[connection.params]
host = "localhost"
username = "root"
"#;

    let file = ConnectionsFile::load_from_str(toml).expect("parse should succeed");
    assert_eq!(file.connections.len(), 1);

    let conn = &file.connections[0];
    assert_eq!(conn.params.ssl_mode, SslMode::Prefer);
    assert!(conn.params.ssl_root_cert.is_none());
    assert!(conn.params.ssl_cert.is_none());
    assert!(conn.params.ssl_key.is_none());
}
