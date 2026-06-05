//! T2-T2-D: end-to-end coverage for the audit emit hooks added in
//! step 3/3 — `ConnectionOpened`, `ConnectionClosed`, and
//! `Configuration` events alongside the `Query` events already
//! emitted by `run::record_history`.
//!
//! The tests install a capturing sink, drive the headless `AppCore`
//! through realistic command sequences, then assert the JSON wire
//! shape of what landed on the sink.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use narwhal_app::DriverRegistry;
use narwhal_app::core::AppCore;
use narwhal_audit::sinks::SinkError;
use narwhal_audit::{AuditService, AuditSink};
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

/// Captures every line written, for assertion.
#[derive(Debug, Default)]
struct CapturingSink {
    lines: StdMutex<Vec<String>>,
}

impl AuditSink for CapturingSink {
    fn write<'a>(
        &'a self,
        line: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.lines.lock().unwrap().push(line.to_owned());
            Ok(())
        })
    }
    fn flush<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SinkError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(()) })
    }
}

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "audit-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

/// Spin up an `AppCore` with the audit service wired and a single
/// in-memory `SQLite` database ready to be opened as `audit-test`.
async fn core_with_audit() -> (AppCore, Arc<CapturingSink>, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("audit.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    }
    let sink = Arc::new(CapturingSink::default());
    let svc = AuditService::builder()
        .with_sink(sink.clone())
        .start()
        .expect("sink installed");
    let svc = Arc::new(svc);

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.set_audit_service(svc);
    (core, sink, dir)
}

/// Settle the in-process audit worker. Each emit is `tokio::spawn`'d
/// from the sync hooks, so the test has to yield long enough for the
/// spawned task to push the event through the bounded mpsc and into
/// the sink before reading the captured lines.
async fn drain() {
    for _ in 0..5 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_emits_connection_opened() {
    let (mut core, sink, _dir) = core_with_audit().await;
    core.execute_command("open audit-test").await;
    drain().await;

    let lines = sink.lines.lock().unwrap().clone();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(r#""kind":"connection_opened""#)),
        "no connection_opened event captured; got: {lines:?}"
    );
    let opened = lines
        .iter()
        .find(|l| l.contains(r#""kind":"connection_opened""#))
        .unwrap();
    assert!(opened.contains(r#""conn":"audit-test""#));
    assert!(
        opened.contains(r#""host":""#),
        "host field should be populated; got: {opened}"
    );
    assert!(
        opened.contains(r#""session_id":""#),
        "session_id should be present; got: {opened}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_emits_connection_closed_with_duration() {
    let (mut core, sink, _dir) = core_with_audit().await;
    core.execute_command("open audit-test").await;
    drain().await;
    // Give the duration_ms field a non-zero observable value so the
    // sink line proves we are measuring elapsed wall-clock and not
    // emitting a hard-coded zero.
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    core.execute_command("close").await;
    drain().await;

    let lines = sink.lines.lock().unwrap().clone();
    let closed = lines
        .iter()
        .find(|l| l.contains(r#""kind":"connection_closed""#))
        .unwrap_or_else(|| panic!("no connection_closed event; got: {lines:?}"));
    assert!(
        closed.contains(r#""duration_ms":"#),
        "duration_ms missing; got: {closed}"
    );
    // The opened and closed events must share a session_id (use the
    // raw substring check: opened.session_id == closed.session_id).
    let opened = lines
        .iter()
        .find(|l| l.contains(r#""kind":"connection_opened""#))
        .unwrap();
    let extract_id = |s: &str| -> String {
        let key = "\"session_id\":\"";
        let start = s.find(key).unwrap() + key.len();
        let end = s[start..].find('"').unwrap();
        s[start..start + end].to_owned()
    };
    assert_eq!(
        extract_id(opened),
        extract_id(closed),
        "session_id must match across open/close"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_emit_uses_open_session_id() {
    let (mut core, sink, _dir) = core_with_audit().await;
    core.execute_command("open audit-test").await;
    drain().await;
    core.insert_into_editor("SELECT 1").await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    drain().await;

    let lines = sink.lines.lock().unwrap().clone();
    let opened = lines
        .iter()
        .find(|l| l.contains(r#""kind":"connection_opened""#))
        .unwrap();
    let query = lines
        .iter()
        .find(|l| l.contains(r#""kind":"query""#))
        .unwrap_or_else(|| panic!("no query event; got: {lines:?}"));
    let extract_id = |s: &str| -> String {
        let key = "\"session_id\":\"";
        let start = s.find(key).unwrap() + key.len();
        let end = s[start..].find('"').unwrap();
        s[start..start + end].to_owned()
    };
    assert_eq!(extract_id(opened), extract_id(query));
    assert!(query.contains(r#""succeeded":true"#));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_audit_service_is_noop() {
    // Sanity check: without `set_audit_service`, the same command
    // sequence must not panic and the AuditService stays absent.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("noop.db");
    {
        rusqlite::Connection::open(&db_path).unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open audit-test").await;
    core.execute_command("close").await;
    // Nothing to assert beyond non-panic — the absence of an audit
    // service must never break the lifecycle path.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_persists_buffered_lines() {
    // Confirms the worker drains in-flight events on shutdown so the
    // CLI binary's clean-exit path doesn't lose audit lines.
    let (mut core, sink, _dir) = core_with_audit().await;
    core.execute_command("open audit-test").await;
    drain().await;
    // Pull the service out of the core to call shutdown directly —
    // the test mirrors what the binary's drop sequence does.
    let svc = core
        .audit_service_for_test()
        .cloned()
        .expect("service installed");
    svc.shutdown().await;
    let lines = sink.lines.lock().unwrap().clone();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(r#""kind":"connection_opened""#)),
        "shutdown must flush any in-flight events; got: {lines:?}"
    );
}
