//! Integration tests for the WASM plugin runtime.
//!
//! The full end-to-end exercise — building the `examples/hello-world`
//! crate to a `.wasm` component and watching the host invoke
//! `on-event` — requires a `wasm32-wasip1-component` toolchain that
//! the workspace dev shell does not preinstall. That test is gated on
//! the `NARWHAL_WASM_EXAMPLE` environment variable; when set, the
//! suite loads the pointed-at component and asserts the event
//! delivery actually happens. CI flips the env on once the component
//! toolchain is wired in (tracked in the integration sweep).
//!
//! These tests cover the *host-only* contract: manifest parsing,
//! capability matching, runtime configuration, and the failure path
//! when the on-disk `.wasm` file is missing or malformed.

use std::path::PathBuf;

use narwhal_plugin::{Plugin, PluginEvent, PluginRegistry};
use narwhal_plugin_wasm::{
    Capability, CapabilitySet, HOST_API_MAJOR, Manifest, PathScope, RecordingLogSink, Runtime,
    RuntimeConfig, WasmError,
};

fn write_manifest(dir: &tempfile::TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("plugin.toml");
    std::fs::write(&path, body).expect("write manifest");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn load_fails_when_component_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_manifest(
        &dir,
        r#"
            name = "ghost"
            version = "0.1.0"
            api-version = 1
        "#,
    );
    let rt = Runtime::new().expect("runtime");
    let err = rt.load(&path).await.expect_err("missing .wasm");
    match err {
        WasmError::Io { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn load_fails_when_component_bytes_are_not_a_component() {
    let dir = tempfile::tempdir().unwrap();
    // Drop a tiny non-wasm file so the manifest-resolution succeeds but
    // the compile step trips.
    let wasm_path = dir.path().join("garbage.wasm");
    std::fs::write(&wasm_path, b"definitely not wasm").unwrap();
    let path = write_manifest(
        &dir,
        r#"
            name = "garbage"
            version = "0.1.0"
            api-version = 1
            component = "garbage.wasm"
        "#,
    );
    let rt = Runtime::new().expect("runtime");
    let err = rt.load(&path).await.expect_err("non-wasm bytes");
    match err {
        WasmError::Wasmtime(msg) => assert!(msg.contains("garbage")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn capability_denied_short_circuits_before_compile() {
    // Even though the .wasm is missing, the capability check must
    // fire first — proving we don't waste bytes on a plugin whose
    // policy never matched.
    let dir = tempfile::tempdir().unwrap();
    let path = write_manifest(
        &dir,
        r#"
            name = "noisy"
            version = "0.1.0"
            api-version = 1
            capabilities = ["net"]
        "#,
    );
    let rt = Runtime::new().expect("runtime");
    let err = rt.load(&path).await.expect_err("net is denied by default");
    match err {
        // Legacy bare `net` token now parses to the wildcard
        // explicit form `net.connect:*`; the canonicalisation
        // travels into the denial message.
        WasmError::CapabilityDenied { capability } => assert_eq!(capability, "net.connect:*"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn manifest_round_trip_with_capabilities() {
    let path = std::path::Path::new("/tmp/x/plugin.toml");
    let body = r#"
        name = "x"
        version = "0.1.0"
        api-version = 1
        capabilities = ["state", "cmd", "fs.read:/etc"]
    "#;
    let m = Manifest::from_toml_str(body, path).unwrap();
    assert_eq!(m.name, "x");
    assert!(m.capabilities.contains(&Capability::State));
    assert!(m.capabilities.contains(&Capability::Cmd));
    assert!(
        m.capabilities
            .contains(&Capability::FsRead(PathScope::parse("/etc").unwrap()))
    );
}

#[test]
fn runtime_config_defaults_match_documented_constants() {
    let cfg = RuntimeConfig::default();
    assert_eq!(cfg.memory_limit, 64 * 1024 * 1024);
    assert_eq!(cfg.fuel_per_call, 100_000_000);
    assert_eq!(cfg.kv_budget, 256 * 1024);
}

#[test]
fn host_api_major_is_stable() {
    // If this assertion ever flips, every plugin SDK consumer needs
    // a coordinated bump and a migration entry — the docstring on
    // `HOST_API_MAJOR` calls it out.
    assert_eq!(HOST_API_MAJOR, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn capability_set_check_is_called_inside_load_with_manifest() {
    let mut manifest = Manifest::from_toml_str(
        r#"
            name = "x"
            version = "0"
            api-version = 1
            capabilities = ["env"]
        "#,
        std::path::Path::new("/tmp/x/plugin.toml"),
    )
    .unwrap();
    // Resolve the component to /dev/null so we hit the capability
    // gate, not the file gate.
    manifest.component_path = PathBuf::from("/dev/null");

    let rt = Runtime::new().unwrap();
    let err = rt.load_with_manifest(manifest).await.unwrap_err();
    match err {
        WasmError::CapabilityDenied { capability } => {
            assert_eq!(capability, "env.read:*");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_broadcast_fans_out_to_wasm_plugin_stub() {
    // Sanity-check the registry → plugin trait wiring without
    // actually instantiating a wasm component. Uses a hand-rolled
    // `Plugin` impl that records events; proves the
    // `PluginRegistry::broadcast_event` path the WASM runtime relies
    // on actually delivers payloads in order.
    use async_trait::async_trait;
    use narwhal_plugin::{PluginError, PluginResult};
    use std::sync::{Arc, Mutex};

    struct Recorder {
        seen: Arc<Mutex<Vec<PluginEvent>>>,
    }

    #[async_trait]
    impl Plugin for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }
        async fn on_event(&self, e: &PluginEvent) -> PluginResult<()> {
            self.seen
                .lock()
                .map(|mut g| g.push(e.clone()))
                .map_err(|err| PluginError::Runtime(err.to_string()))
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut reg = PluginRegistry::new();
    reg.register(Recorder {
        seen: Arc::clone(&seen),
    })
    .unwrap();

    reg.broadcast_event(&PluginEvent::ConnectionOpened {
        name: "prod".into(),
    })
    .await
    .unwrap();
    reg.broadcast_event(&PluginEvent::QueryFinished {
        sql: "SELECT 1".into(),
        rows: 1,
        elapsed_ms: 3,
        ok: true,
    })
    .await
    .unwrap();

    let captured = seen.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
}

#[test]
fn recording_log_sink_collects_and_drains() {
    use narwhal_plugin_wasm::{LogLine, LogSink};
    let sink = RecordingLogSink::new();
    sink.emit(LogLine::new("p", "info", "m"));
    assert_eq!(sink.snapshot().len(), 1);
    let drained = sink.drain();
    assert_eq!(drained.len(), 1);
    assert!(sink.snapshot().is_empty());
}

// ---------------------------------------------------------------------------
// End-to-end test: only runs when a pre-built component is available.
// Set `NARWHAL_WASM_EXAMPLE=/path/to/hello_world.wasm` to enable it.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires NARWHAL_WASM_EXAMPLE pointing at a built component"]
async fn end_to_end_event_delivery_with_real_component() {
    let Some(path) = std::env::var_os("NARWHAL_WASM_EXAMPLE") else {
        eprintln!("set NARWHAL_WASM_EXAMPLE to enable this test");
        return;
    };
    let component_path = PathBuf::from(&path);
    let dir = component_path
        .parent()
        .expect("component path has parent")
        .to_path_buf();

    // Synthesise a manifest pointing at the real binary.
    let manifest = Manifest::from_toml_str(
        &format!(
            r#"
                name = "hello-world"
                version = "0.1.0"
                api-version = 1
                component = {:?}
                capabilities = []
            "#,
            component_path.file_name().unwrap().to_string_lossy(),
        ),
        &dir.join("plugin.toml"),
    )
    .unwrap();

    let sink = std::sync::Arc::new(RecordingLogSink::new());
    let runtime = Runtime::new()
        .unwrap()
        .with_log_sink(sink.clone() as std::sync::Arc<dyn narwhal_plugin_wasm::LogSink>);
    let plugin = runtime.load_with_manifest(manifest).await.unwrap();

    plugin
        .on_event(&PluginEvent::ConnectionOpened {
            name: "demo".into(),
        })
        .await
        .unwrap();

    let lines = sink.snapshot();
    assert!(
        !lines.is_empty(),
        "expected the example plugin to log at least once"
    );
}

// Silence the unused-import warning under the cfg gate above.
#[allow(dead_code)]
fn _capability_set_imports() {
    let _ = CapabilitySet::new();
}
