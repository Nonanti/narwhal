//! Integration tests for T1-T2-B vault providers.
//!
//! Two providers, two test surfaces:
//!
//! * **`HashiCorp`**: stand up a minimal HTTP mock server on a
//!   loopback `TcpListener` that responds with a canned KV v2 JSON
//!   payload, and point `HashicorpVault` at it. No external network,
//!   no docker dependency — the test runs anywhere `tokio` does.
//!
//! * **1Password**: write a shell script that prints a canned secret
//!   to stdout, and set
//!   [`OnePasswordVaultSettings::op_binary`](narwhal_config::OnePasswordVaultSettings::op_binary)
//!   to its path. This is exactly the "mock mode" the brief's
//!   acceptance criterion calls for.
//!
//! Plus the end-to-end orchestrator test:
//! [`narwhal_config::resolve_connection_password`] dispatches to the
//! right layer based on `ConnectionParams::password` shape.

// Edition 2024 marks `std::env::set_var` as `unsafe` (race with libc
// `getenv` in other threads). The vault provider tests set a few
// `NARWHAL_VAULT_TEST_TOKEN_*` vars before invoking the HTTP client;
// the same opt-out pattern lives in `tests/pgpass_env.rs`. Library
// code keeps `#![forbid(unsafe_code)]` at the lib root.
#![allow(unsafe_code)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use narwhal_config::settings::{
    HashicorpVaultSettings, OnePasswordVaultSettings, VaultProviderSettings, VaultSettings,
};
use narwhal_config::{
    DynCredentialStore, InMemoryStore, Reference, VaultError, VaultRegistry,
    resolve_connection_password,
    vault::{HashicorpVault, OnepasswordCli},
};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use secrecy::{ExposeSecret, SecretString};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Spin up a tiny HTTP mock on `127.0.0.1:0` that responds to every
/// request with `status` + `body`. Returns the bound socket address
/// and a handle that aborts the server when dropped.
///
/// The handler is intentionally trivial — it reads up to a
/// `\r\n\r\n` head, ignores the request, and writes a fixed
/// response. Concurrency: spawns one accept loop, one task per
/// connection.
async fn spawn_mock_http(
    status: u16,
    status_text: &'static str,
    body: &'static str,
    handler_calls: Arc<AtomicUsize>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            handler_calls.fetch_add(1, Ordering::SeqCst);
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {len}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body}",
                len = body.len()
            );
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                // Drain the request head; ignore content. Short
                // reads are fine — we never act on the request.
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (addr, handle)
}

fn make_hashicorp(addr: std::net::SocketAddr, token_env: &str) -> HashicorpVault {
    let settings = HashicorpVaultSettings::with(|s| {
        s.address = Some(format!("http://{addr}"));
        s.token_env = Some(token_env.into());
        s.timeout_secs = Some(2);
    });
    HashicorpVault::from_settings("hashicorp", &settings).unwrap()
}

/// Set a unique env var for a single test so concurrent test
/// processes don't trip over each other. Returns the var name.
fn set_test_token_env(suffix: &str) -> String {
    let name = format!("NARWHAL_VAULT_TEST_TOKEN_{suffix}");
    // SAFETY: integration tests run in their own process. Even so,
    // setting env vars is `unsafe` in edition 2024 because libc
    // `getenv` is non-reentrant. We accept the risk in the test
    // crate (which already needs `unsafe_code` allowed below).
    unsafe { std::env::set_var(&name, "test-vault-token") };
    name
}

#[tokio::test]
async fn hashicorp_resolves_field_from_kv_v2_payload() {
    let body = r#"{"data":{"data":{"password":"hunter2","username":"alice"}}}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(200, "OK", body, calls.clone()).await;
    let token_env = set_test_token_env("RESOLVES_FIELD");
    let provider = make_hashicorp(addr, &token_env);
    let reference = Reference::try_parse("vault:hashicorp/secret/data/db/prod#password")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let secret = registry.resolve(&reference).await.unwrap();
    assert_eq!(secret.expose_secret(), "hunter2");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn hashicorp_single_field_path_returns_only_value_without_selector() {
    let body = r#"{"data":{"data":{"only":"value"}}}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(200, "OK", body, calls.clone()).await;
    let token_env = set_test_token_env("SINGLE_FIELD");
    let provider = make_hashicorp(addr, &token_env);
    let reference = Reference::try_parse("vault:hashicorp/secret/data/x")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let secret = registry.resolve(&reference).await.unwrap();
    assert_eq!(secret.expose_secret(), "value");
}

#[tokio::test]
async fn hashicorp_404_classified_as_not_found() {
    let body = r#"{"errors":["not found"]}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(404, "Not Found", body, calls.clone()).await;
    let token_env = set_test_token_env("NOT_FOUND");
    let provider = make_hashicorp(addr, &token_env);
    let reference = Reference::try_parse("vault:hashicorp/secret/data/missing#p")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let err = registry.resolve(&reference).await.unwrap_err();
    match err {
        VaultError::NotFound { reference } => assert!(reference.contains("missing")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn hashicorp_unreachable_when_address_refused() {
    // Bind, then drop the listener so the port is free; the
    // subsequent connect attempt fails fast.
    let token_env = set_test_token_env("UNREACHABLE");
    // SAFETY: see set_test_token_env.
    unsafe { std::env::set_var(&token_env, "tok") };
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
        // listener drops here
    };
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let provider = make_hashicorp(addr, &token_env);
    let reference = Reference::try_parse("vault:hashicorp/secret/data/x#p")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let err = registry.resolve(&reference).await.unwrap_err();
    assert!(
        matches!(
            err,
            VaultError::Unreachable { .. } | VaultError::BadResponse { .. }
        ),
        "expected Unreachable or BadResponse, got {err:?}",
    );
}

#[tokio::test]
async fn hashicorp_concurrent_resolves_dedup_to_one_http_call() {
    // Slow response so all 8 callers pile up before the leader
    // finishes. Body contains the same single field every time.
    let body = r#"{"data":{"data":{"p":"shared-secret"}}}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(200, "OK", body, calls.clone()).await;
    let token_env = set_test_token_env("DEDUP");
    let provider = make_hashicorp(addr, &token_env);
    let reference = Reference::try_parse("vault:hashicorp/secret/data/x#p")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let registry = Arc::new(registry);
    let reference = Arc::new(reference);

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let reg = registry.clone();
        let r = reference.clone();
        tasks.push(tokio::spawn(async move { reg.resolve(&r).await }));
    }
    let results = futures::future::join_all(tasks).await;
    for r in results {
        let secret = r.unwrap().unwrap();
        assert_eq!(secret.expose_secret(), "shared-secret");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "8 concurrent resolves must coalesce into 1 HTTP call",
    );
}

#[tokio::test]
async fn hashicorp_token_env_unset_errors_clearly() {
    let body = r#"{"data":{"data":{"p":"x"}}}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(200, "OK", body, calls.clone()).await;
    // Deliberately use a token env var we never set.
    let provider = make_hashicorp(addr, "DEFINITELY_NOT_SET_NARWHAL_TEST_TOKEN_XYZ");
    let reference = Reference::try_parse("vault:hashicorp/secret/data/x#p")
        .unwrap()
        .unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let err = registry.resolve(&reference).await.unwrap_err();
    assert!(matches!(err, VaultError::NotConfigured { .. }));
}

// ----- 1Password CLI mock -------------------------------------------------

fn write_op_stub(tmp: &std::path::Path, body: &str) -> std::path::PathBuf {
    let script = tmp.join("op-stub.sh");
    // Print body verbatim with a trailing newline, just like real `op`.
    let contents = format!("#!/bin/sh\n# Test stub for 1Password CLI.\necho '{body}'\n");
    std::fs::write(&script, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    script
}

fn write_op_failing_stub(tmp: &std::path::Path, stderr: &str, exit: u8) -> std::path::PathBuf {
    let script = tmp.join("op-stub-fail.sh");
    let contents =
        format!("#!/bin/sh\n# Test stub that fails.\necho '{stderr}' >&2\nexit {exit}\n");
    std::fs::write(&script, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    script
}

#[tokio::test]
#[cfg(unix)] // The shell-stub trick is Unix-specific.
async fn onepassword_resolves_via_stub_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_op_stub(tmp.path(), "from-1password");
    let settings = OnePasswordVaultSettings::with(|s| {
        s.op_binary = Some(stub.to_string_lossy().into_owned());
        s.timeout_secs = Some(2);
    });
    let provider = OnepasswordCli::from_settings("1password", &settings).unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let reference = Reference::try_parse("1password:op://Vault/Item/password")
        .unwrap()
        .unwrap();
    let secret = registry.resolve(&reference).await.unwrap();
    assert_eq!(secret.expose_secret(), "from-1password");
}

#[tokio::test]
#[cfg(unix)]
async fn onepassword_missing_item_classified_as_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_op_failing_stub(tmp.path(), "item not found", 1);
    let settings = OnePasswordVaultSettings::with(|s| {
        s.op_binary = Some(stub.to_string_lossy().into_owned());
        s.timeout_secs = Some(2);
    });
    let provider = OnepasswordCli::from_settings("1password", &settings).unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let reference = Reference::try_parse("1password:op://Vault/Missing/password")
        .unwrap()
        .unwrap();
    let err = registry.resolve(&reference).await.unwrap_err();
    assert!(matches!(err, VaultError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn onepassword_binary_not_found_reported_as_unreachable() {
    let settings = OnePasswordVaultSettings::with(|s| {
        s.op_binary = Some("/this/path/definitely/does/not/exist/op".into());
        s.timeout_secs = Some(2);
    });
    let provider = OnepasswordCli::from_settings("1password", &settings).unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let reference = Reference::try_parse("1password:op://V/I/p")
        .unwrap()
        .unwrap();
    let err = registry.resolve(&reference).await.unwrap_err();
    assert!(matches!(err, VaultError::Unreachable { .. }), "got {err:?}");
}

#[tokio::test]
#[cfg(unix)]
async fn onepassword_service_account_env_pre_check_fires() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_op_stub(tmp.path(), "would-never-run");
    let settings = OnePasswordVaultSettings::with(|s| {
        s.op_binary = Some(stub.to_string_lossy().into_owned());
        s.timeout_secs = Some(2);
        s.service_account_token_env = Some("NARWHAL_TEST_DEFINITELY_NOT_SET_OP_TOKEN_XYZ".into());
    });
    let provider = OnepasswordCli::from_settings("1password", &settings).unwrap();
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));
    let reference = Reference::try_parse("1password:op://V/I/p")
        .unwrap()
        .unwrap();
    let err = registry.resolve(&reference).await.unwrap_err();
    assert!(
        matches!(err, VaultError::NotConfigured { .. }),
        "got {err:?}"
    );
}

// ----- End-to-end orchestrator -------------------------------------------

fn make_config_with_password(password: Option<&str>) -> ConnectionConfig {
    let params = ConnectionParams::with(|p| {
        p.host = Some("localhost".into());
        p.port = Some(5432);
        p.database = Some("appdb".into());
        p.username = Some("alice".into());
        p.password = password.map(str::to_owned);
    });
    ConnectionConfig {
        id: uuid::Uuid::new_v4(),
        name: "test".into(),
        driver: "postgres".into(),
        params,
    }
}

#[tokio::test]
async fn orchestrator_inline_literal_is_used_verbatim() {
    let config = make_config_with_password(Some("inline-pw"));
    let secret = resolve_connection_password(&config, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secret.expose_secret(), "inline-pw");
}

#[tokio::test]
async fn orchestrator_empty_inline_does_not_mask_keyring() {
    let config = make_config_with_password(Some(""));
    let keyring = InMemoryStore::new();
    keyring
        .set(config.id, SecretString::new("from-keyring".into()))
        .await
        .unwrap();
    let secret = resolve_connection_password(&config, None, Some(&keyring))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secret.expose_secret(), "from-keyring");
}

#[tokio::test]
async fn orchestrator_keyring_used_when_no_inline_password() {
    let config = make_config_with_password(None);
    let keyring = InMemoryStore::new();
    keyring
        .set(config.id, SecretString::new("kr-pw".into()))
        .await
        .unwrap();
    let secret = resolve_connection_password(&config, None, Some(&keyring))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secret.expose_secret(), "kr-pw");
}

#[tokio::test]
async fn orchestrator_vault_reference_dispatches_to_registry() {
    let body = r#"{"data":{"data":{"password":"vault-pw"}}}"#;
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(200, "OK", body, calls.clone()).await;
    let token_env = set_test_token_env("ORCH_VAULT");
    let provider = make_hashicorp(addr, &token_env);
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));

    let config = make_config_with_password(Some("vault:hashicorp/secret/data/db/prod#password"));
    let secret = resolve_connection_password(&config, Some(&registry), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(secret.expose_secret(), "vault-pw");
}

#[tokio::test]
async fn orchestrator_vault_reference_without_registry_errors() {
    let config = make_config_with_password(Some("vault:hashicorp/secret/data/db/prod#password"));
    let err = resolve_connection_password(&config, None, None)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("vault"),
        "expected vault-flavoured error, got {msg}"
    );
}

#[tokio::test]
async fn orchestrator_vault_does_not_fall_through_to_keyring_on_failure() {
    // Brief tricky bit: when the user has opted in to vault storage,
    // a vault failure must NOT silently fall back to a stale keyring
    // entry — that would defeat the security intent.
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, _server) = spawn_mock_http(
        403,
        "Forbidden",
        r#"{"errors":["permission denied"]}"#,
        calls,
    )
    .await;
    let token_env = set_test_token_env("NO_FALLBACK");
    let provider = make_hashicorp(addr, &token_env);
    let registry = VaultRegistry::empty().with_provider(Arc::new(provider));

    let config = make_config_with_password(Some("vault:hashicorp/secret/data/x#p"));
    let keyring = InMemoryStore::new();
    // Even though the keyring HAS an entry, vault failure must surface.
    keyring
        .set(config.id, SecretString::new("stale-from-keyring".into()))
        .await
        .unwrap();
    let result = resolve_connection_password(&config, Some(&registry), Some(&keyring)).await;
    assert!(
        result.is_err(),
        "vault denial must NOT fall through to keyring"
    );
}

#[tokio::test]
async fn orchestrator_malformed_vault_reference_errors() {
    let config = make_config_with_password(Some("vault:")); // empty body
    let err = resolve_connection_password(&config, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("vault"));
}

// ----- Cancellation contract ---------------------------------------------

#[tokio::test]
async fn cancelled_waiter_does_not_break_leader() {
    // Stub provider with a long delay so we can drop a waiter while
    // the leader is still in flight. Then a subsequent resolve
    // should re-use the same leader's result without re-running.
    #[derive(Debug)]
    struct SlowStub {
        delay: Duration,
        calls: Arc<AtomicUsize>,
    }
    impl narwhal_config::vault::VaultProvider for SlowStub {
        fn name(&self) -> &str {
            "hashicorp"
        }
        fn resolve<'a>(
            &'a self,
            _reference: &'a Reference,
        ) -> futures::future::BoxFuture<'a, Result<Arc<SecretString>, VaultError>> {
            let calls = self.calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(self.delay).await;
                Ok(Arc::new(SecretString::new("late".into())))
            })
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let stub = SlowStub {
        delay: Duration::from_millis(150),
        calls: calls.clone(),
    };
    let registry = Arc::new(VaultRegistry::empty().with_provider(Arc::new(stub)));
    let reference = Arc::new(
        Reference::try_parse("vault:hashicorp/secret/data/x#p")
            .unwrap()
            .unwrap(),
    );

    let leader = {
        let r = reference.clone();
        let reg = registry.clone();
        tokio::spawn(async move { reg.resolve(&r).await })
    };
    let waiter = {
        let r = reference.clone();
        let reg = registry.clone();
        tokio::spawn(async move { reg.resolve(&r).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    waiter.abort();
    let _ = waiter.await; // join, ignore aborted err

    let result = leader.await.unwrap().unwrap();
    assert_eq!(result.expose_secret(), "late");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "leader must run exactly once"
    );
}

// ----- VaultSettings -> VaultRegistry --------------------------------------

#[tokio::test]
async fn from_settings_round_trip_with_both_providers() {
    let token_env = set_test_token_env("FROM_SETTINGS");
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_op_stub(tmp.path(), "x");
    let providers = VaultProviderSettings::with(|p| {
        p.hashicorp = Some(HashicorpVaultSettings::with(|s| {
            s.address = Some("http://127.0.0.1:1".into());
            s.token_env = Some(token_env);
        }));
        p.onepassword = Some(OnePasswordVaultSettings::with(|s| {
            s.op_binary = Some(stub.to_string_lossy().into_owned());
        }));
    });
    let vault = VaultSettings::with(|v| v.providers = providers);
    let registry = VaultRegistry::from_settings(&vault).unwrap();
    let mut names = registry.provider_names();
    names.sort_unstable();
    assert_eq!(names, vec!["1password", "hashicorp"]);
}
