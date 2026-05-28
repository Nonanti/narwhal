//! Settings × Grants interaction tests.
//!
//! Two layered policies must agree before a manifest's declared
//! capability lands in the effective set:
//!
//! 1. **Coarse settings** ([`narwhal_config::WasmPluginSettings`]) —
//! bool flags. Reflects a Tier-0 v2 setting most operators will
//! edit by hand.
//! 2. **Fine grants** ([`narwhal_plugin_wasm::Grants`]) — typed
//! capability list. Reflects an explicit
//! `[[plugins.grants]]`-style allow-list. Embedders construct
//! these directly until the Tier-2 settings parser catches up.
//!
//! The runtime intersects manifest ∩ settings ∩ grants. Each of
//! the three layers can short-circuit a load.

use std::path::PathBuf;

use narwhal_config::WasmPluginSettings;
use narwhal_plugin_wasm::{
    Capability, EnvVar, Grants, HostPort, Manifest, PathScope, Runtime, RuntimeConfig, WasmError,
};

fn manifest(capabilities: &str) -> Manifest {
    let body = format!(
        r#"
            name = "x"
            version = "0.1.0"
            api-version = 1
            capabilities = {capabilities}
        "#
    );
    let mut m = Manifest::from_toml_str(&body, std::path::Path::new("/tmp/x/plugin.toml")).unwrap();
    m.component_path = PathBuf::from("/dev/null");
    m
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_flag_disabled_blocks_manifest_load() {
    // Default settings have allow_fs_read=false, so even a perfectly
    // valid `fs.read:/etc` manifest is refused at the coarse gate.
    let cfg = RuntimeConfig::default();
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(r#"["fs.read:/etc"]"#))
        .await
        .unwrap_err();
    assert!(matches!(err, WasmError::CapabilityDenied { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_enabled_then_fine_grants_required() {
    // Coarse gate open (allow_fs_read=true) but fine grants empty —
    // load still fails because the fine layer denies.
    let mut cfg = RuntimeConfig::default();
    let mut sp = WasmPluginSettings::default();
    sp.enabled = true;
    sp.allow_fs_read = true;
    cfg.settings_policy = sp;
    cfg.grants = Grants::deny_all(); // explicit
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(r#"["fs.read:/etc"]"#))
        .await
        .unwrap_err();
    match err {
        WasmError::CapabilityDenied { capability } => assert_eq!(capability, "fs.read:/etc"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fine_grants_narrower_than_manifest_denies_at_load() {
    let mut cfg = RuntimeConfig::default();
    let mut sp = WasmPluginSettings::default();
    sp.enabled = true;
    sp.allow_fs_read = true;
    cfg.settings_policy = sp;
    // Fine grant only covers /home, not /etc.
    cfg.grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/home").unwrap())]);
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(r#"["fs.read:/etc"]"#))
        .await
        .unwrap_err();
    match err {
        WasmError::CapabilityDenied { capability } => assert_eq!(capability, "fs.read:/etc"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fine_grants_covering_manifest_passes_capability_gate() {
    // Even though the .wasm doesn't exist, the capability gates
    // must clear so we hit the file-not-found path. Confirms both
    // layers see and pass the manifest.
    let mut cfg = RuntimeConfig::default();
    let mut sp = WasmPluginSettings::default();
    sp.enabled = true;
    sp.allow_fs_read = true;
    cfg.settings_policy = sp;
    cfg.grants = Grants::from_caps([Capability::FsRead(PathScope::parse("/etc").unwrap())]);
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(r#"["fs.read:/etc/passwd"]"#))
        .await
        .unwrap_err();
    // /etc grant covers /etc/passwd → both gates pass → load fails
    // *past* the capability gates (file does not parse as a wasm
    // component; the exact downstream variant depends on whether
    // /dev/null is treated as zero-byte or as a present-but-bad
    // file, hence the Io|Wasmtime accept set).
    assert!(
        matches!(err, WasmError::Io { .. } | WasmError::Wasmtime(_)),
        "expected capability gate to pass, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_unit_tokens_still_load_with_settings_only() {
    // The on-disk manifest uses unit tokens. Operators who
    // upgraded their settings (allow_fs_read=true) should still see
    // those manifests load — Grants::from_settings exposes the same
    // mapping the load-time check uses.
    let mut cfg = RuntimeConfig::default();
    let mut sp = WasmPluginSettings::default();
    sp.enabled = true;
    sp.allow_fs_read = true;
    sp.allow_net = true;
    sp.allow_env = true;
    cfg.settings_policy = sp;
    cfg.grants = RuntimeConfig::grants_from_settings(&cfg.settings_policy);
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(r#"["fs-read", "net", "env"]"#))
        .await
        .unwrap_err();
    // All three caps clear; load fails past the capability gates.
    assert!(
        matches!(err, WasmError::Io { .. } | WasmError::Wasmtime(_)),
        "expected capability gate to pass, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn open_all_lets_every_manifest_through_capability_gate() {
    let mut cfg = RuntimeConfig::default();
    let mut sp = WasmPluginSettings::default();
    sp.enabled = true;
    sp.allow_fs_read = true;
    sp.allow_fs_write = true;
    sp.allow_net = true;
    sp.allow_env = true;
    cfg.settings_policy = sp;
    cfg.grants = Grants::open_all();
    let rt = Runtime::with_config(cfg).unwrap();
    let err = rt
        .load_with_manifest(manifest(
            r#"["state", "cmd", "fs.read:/etc", "fs.write:/tmp", "net.connect:api.test:443", "env.read:HOME"]"#,
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WasmError::Io { .. } | WasmError::Wasmtime(_)),
        "expected capability gate to pass, got {err:?}"
    );
}

#[test]
fn grants_from_settings_helper_mirrors_load_time_mapping() {
    // The helper is what embedders invoke when they don't have a
    // hand-written Grants list. Round-trip a representative set so
    // the documented mapping stays stable.
    let mut s = WasmPluginSettings::default();
    s.enabled = true;
    s.allow_fs_read = true;
    s.allow_net = true;
    let g = RuntimeConfig::grants_from_settings(&s);
    assert!(g.covers(&Capability::FsRead(PathScope::parse("/anywhere").unwrap())));
    assert!(g.covers(&Capability::NetConnect(
        HostPort::parse("x.test:1").unwrap()
    )));
    assert!(!g.covers(&Capability::FsWrite(PathScope::parse("/tmp").unwrap())));
    assert!(!g.covers(&Capability::EnvRead(EnvVar::parse("HOME").unwrap())));
    assert!(g.covers(&Capability::State));
}
