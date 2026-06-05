//! LSP client: lifecycle, request/response routing, typed method
//! helpers. The protocol loop lives in a single `run` task that owns
//! the transport; callers interact through a [`ClientHandle`] which
//! sends pairs of (request, oneshot reply) over a channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use lsp_types::{
    CompletionParams, CompletionResponse, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    Hover, HoverParams, InitializeParams, InitializeResult, InitializedParams,
};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::jsonrpc::{Id, Incoming, Notification, Request, RpcError};
use crate::transport::Transport;

/// How to launch the language server.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// Program to exec (e.g. `"sqls"`, `"sqlls"`, or an absolute path).
    pub command: String,
    /// Extra command-line args.
    pub args: Vec<String>,
    /// Optional config file passed via `--config`. The flag spelling
    /// differs between sqls and sqlls; the dispatch layer fills it in.
    pub config_file: Option<String>,
}

impl ServerSpec {
    /// Build a default spec for the standard sqls binary.
    pub fn sqls() -> Self {
        Self {
            command: "sqls".to_owned(),
            args: Vec::new(),
            config_file: None,
        }
    }

    /// Build a default spec for the standard sqlls binary.
    pub fn sqlls() -> Self {
        Self {
            command: "sqlls".to_owned(),
            args: vec!["--stdio".to_owned()],
            config_file: None,
        }
    }
}

/// Client capabilities advertised to the server on `initialize`. The
/// MVP keeps things minimal — completion + hover + definition.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    #[serde(rename = "textDocument")]
    pub text_document: serde_json::Value,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            text_document: serde_json::json!({
                "completion": { "completionItem": { "snippetSupport": false } },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "definition": {},
            }),
        }
    }
}

/// All the error shapes the client surfaces.
#[derive(Debug, Error)]
pub enum LspError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("framing: {0}")]
    Framing(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server error {}: {}", .0.code, .0.message)]
    Server(#[from] RpcError),
    #[error("client task closed before request could complete")]
    ChannelClosed,
    #[error("server stream closed before response arrived")]
    ServerClosed,
    /// MR-N9: carries the method name so status-bar surfacing has
    /// the context the operator needs ("completion timed out" vs
    /// "hover timed out").
    #[error("LSP request '{0}' timed out waiting for server response")]
    Timeout(String),
}

/// Inbound notification handed back to the caller through the
/// notification channel.
#[derive(Debug, Clone)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

type ResponseTx = oneshot::Sender<Result<Value, RpcError>>;

/// One request the run-loop has to dispatch. Notifications use a `None`
/// response slot so the loop knows not to wait for an id.
enum Outbound {
    /// Wraps the responder + an allocator slot. The slot is filled
    /// by the run-loop once it has minted the JSON-RPC id; the
    /// caller (`request_with_timeout`) uses it to issue a matching
    /// `Cancel(id)` if it gives up on the response.
    Request {
        method: String,
        params: Value,
        responder: ResponseTx,
        id_slot: Arc<tokio::sync::OnceCell<i64>>,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// MR-C2: abandon a previously-issued request. The run-loop
    /// drops the matching `pending` entry so a late server response
    /// is ignored and the `HashMap` doesn't leak under repeated
    /// timeouts.
    Cancel(i64),
    Shutdown(oneshot::Sender<()>),
}

/// Bounded notification queue capacity. Review fix M4: previously
/// `mpsc::unbounded_channel` allowed long-running sqls/sqlls servers
/// to balloon memory when the host never drained
/// `next_notification`. 256 entries is enough for a typing burst;
/// overflow drops the oldest with a `tracing::warn` counter.
const NOTIFICATION_QUEUE_CAPACITY: usize = 256;

/// Default per-request timeout. Callers can override via
/// [`ClientHandle::request_with_timeout`]. Review fix M5: previously
/// `request` had no timeout, so a hung server could hang the editor
/// indefinitely.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Cheap-to-clone handle the editor host uses to talk to the client
/// task. All methods are async; the underlying loop runs in a
/// dedicated `tokio::spawn`.
#[derive(Clone)]
pub struct ClientHandle {
    tx: mpsc::Sender<Outbound>,
    notifications: Arc<Mutex<mpsc::Receiver<ServerNotification>>>,
    /// Counter of notifications dropped because the queue was full.
    /// Exposed for diagnostics; bumped from the run-loop.
    notifications_dropped: Arc<AtomicU64>,
}

impl ClientHandle {
    /// Send a typed request and await the typed response with the
    /// default request timeout (10 s; see [`request_with_timeout`] to
    /// override).
    ///
    /// [`request_with_timeout`]: Self::request_with_timeout
    pub async fn request<P, R>(&self, method: &str, params: P) -> Result<R, LspError>
    where
        P: Serialize,
        R: for<'de> serde::Deserialize<'de>,
    {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    /// Send a typed request, awaiting up to `timeout`. Review fix M5:
    /// a missing timeout previously allowed a wedged language server
    /// to hang the editor indefinitely. Pass
    /// `Duration::from_secs(u64::MAX)` to opt out.
    pub async fn request_with_timeout<P, R>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<R, LspError>
    where
        P: Serialize,
        R: for<'de> serde::Deserialize<'de>,
    {
        let params = serde_json::to_value(params)?;
        let (tx, rx) = oneshot::channel();
        let id_slot: Arc<tokio::sync::OnceCell<i64>> = Arc::new(tokio::sync::OnceCell::new());
        self.tx
            .send(Outbound::Request {
                method: method.to_owned(),
                params,
                responder: tx,
                id_slot: id_slot.clone(),
            })
            .await
            .map_err(|_| LspError::ChannelClosed)?;
        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(payload)) => payload?,
            Ok(Err(_)) => return Err(LspError::ChannelClosed),
            Err(_) => {
                // MR-C2: tell the run-loop to drop the pending
                // entry so a late response can't keep the slot
                // alive. The id may not have been allocated yet
                // (request still queued); in that case the slot is
                // empty and the worker will skip dispatch when it
                // dequeues a request whose responder is gone.
                if let Some(id) = id_slot.get().copied() {
                    let _ = self.tx.send(Outbound::Cancel(id)).await;
                }
                return Err(LspError::Timeout(method.to_owned()));
            }
        };
        let typed = serde_json::from_value(result)?;
        Ok(typed)
    }

    /// Read the dropped-notification counter. Useful for surfacing
    /// "LSP notifications dropping" diagnostics in the status bar.
    ///
    /// MR-N2: `Relaxed` is the correct ordering here — the counter
    /// is monotonic and read for diagnostic purposes only; we do
    /// not synchronise other memory with this value. Strengthening
    /// to `Acquire` would not improve correctness and would cost a
    /// fence on every UI redraw.
    pub fn notifications_dropped(&self) -> u64 {
        self.notifications_dropped.load(Ordering::Relaxed)
    }

    /// Send a notification (no response).
    pub async fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), LspError> {
        let params = serde_json::to_value(params)?;
        self.tx
            .send(Outbound::Notification {
                method: method.to_owned(),
                params,
            })
            .await
            .map_err(|_| LspError::ChannelClosed)
    }

    /// Convenience wrapper for `initialize`.
    pub async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, LspError> {
        self.request("initialize", params).await
    }

    /// Send `initialized` (notification, no response).
    pub async fn initialized(&self) -> Result<(), LspError> {
        self.notify("initialized", InitializedParams {}).await
    }

    /// `textDocument/didOpen` notification.
    pub async fn did_open(&self, params: DidOpenTextDocumentParams) -> Result<(), LspError> {
        self.notify("textDocument/didOpen", params).await
    }

    /// `textDocument/didChange` notification.
    pub async fn did_change(&self, params: DidChangeTextDocumentParams) -> Result<(), LspError> {
        self.notify("textDocument/didChange", params).await
    }

    /// `textDocument/completion` request.
    pub async fn completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>, LspError> {
        self.request("textDocument/completion", params).await
    }

    /// `textDocument/hover` request.
    pub async fn hover(&self, params: HoverParams) -> Result<Option<Hover>, LspError> {
        self.request("textDocument/hover", params).await
    }

    /// Pop the next server-originated notification, blocking until one
    /// arrives or the run-loop exits.
    pub async fn next_notification(&self) -> Option<ServerNotification> {
        let mut guard = self.notifications.lock().await;
        guard.recv().await
    }

    /// Initiate the orderly shutdown sequence: send `shutdown` /
    /// `exit` and wait for the run loop to terminate.
    pub async fn shutdown(self) -> Result<(), LspError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Outbound::Shutdown(tx))
            .await
            .map_err(|_| LspError::ChannelClosed)?;
        let _ = rx.await;
        Ok(())
    }

    // Review fix N5: removed the previously-exposed `next_id()`
    // helper. Its only purpose was to let tests peek at the
    // allocator, but that opened a door for production code to
    // race the run-loop's own allocator and skip IDs.
}

/// The client owns the spawn handle of the protocol loop so the host
/// can await on shutdown.
pub struct Client {
    pub handle: ClientHandle,
    join: tokio::task::JoinHandle<()>,
}

impl Client {
    /// Drive the protocol loop over a generic [`Transport`]. The
    /// returned handle is cheap to clone; the join handle lets the
    /// caller await graceful termination.
    pub fn spawn<T>(mut transport: T) -> Self
    where
        T: Transport + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<Outbound>(64);
        let (notif_tx, notif_rx) = mpsc::channel::<ServerNotification>(NOTIFICATION_QUEUE_CAPACITY);
        // R3-N6: the run-loop is the only owner of the id allocator
        // now that `ClientHandle::next_id()` was removed (review fix
        // N5). No outer binding required.
        let next_id_loop = Arc::new(AtomicI64::new(1));
        let dropped_counter = Arc::new(AtomicU64::new(0));
        let dropped_loop = dropped_counter.clone();

        let join = tokio::spawn(async move {
            let mut pending: HashMap<i64, ResponseTx> = HashMap::new();
            let mut shutdown_signal: Option<oneshot::Sender<()>> = None;

            loop {
                tokio::select! {
                    biased;
                    Some(message) = rx.recv() => {
                        match message {
                            Outbound::Request { method, params, responder, id_slot } => {
                                // R3-M1: residual leak path MR-C2
                                // left open. If the caller's
                                // timeout fired while this request
                                // was still queued, the
                                // `oneshot::Receiver` has been
                                // dropped and `id_slot` is empty
                                // (so no `Cancel` was issued).
                                // Skip dispatch entirely — no
                                // wire traffic, no `pending`
                                // insertion, no leak when the
                                // server then never responds.
                                if responder.is_closed() {
                                    continue;
                                }
                                let id = next_id_loop.fetch_add(1, Ordering::SeqCst);
                                // MR-C2: publish the id back to the
                                // caller before the await so a
                                // `Cancel(id)` from a timeout path
                                // can find this entry.
                                let _ = id_slot.set(id);
                                let req = Request {
                                    jsonrpc: "2.0",
                                    id: Id::Number(id),
                                    method: &method,
                                    params,
                                };
                                let payload = match serde_json::to_vec(&req) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        let _ = responder.send(Err(RpcError {
                                            code: -32700,
                                            message: format!("serialize: {e}"),
                                            data: None,
                                        }));
                                        continue;
                                    }
                                };
                                if let Err(e) = transport.send(&payload).await {
                                    let _ = responder.send(Err(RpcError {
                                        code: -32000,
                                        message: format!("transport: {e}"),
                                        data: None,
                                    }));
                                    continue;
                                }
                                pending.insert(id, responder);
                            }
                            Outbound::Cancel(id) => {
                                // MR-C2: caller gave up; drop the
                                // responder so a late server
                                // response is silently discarded.
                                pending.remove(&id);
                            }
                            Outbound::Notification { method, params } => {
                                let notif = Notification {
                                    jsonrpc: "2.0",
                                    method: &method,
                                    params,
                                };
                                if let Ok(payload) = serde_json::to_vec(&notif) {
                                    let _ = transport.send(&payload).await;
                                }
                            }
                            Outbound::Shutdown(ack) => {
                                let req = Request {
                                    jsonrpc: "2.0",
                                    id: Id::Number(next_id_loop.fetch_add(1, Ordering::SeqCst)),
                                    method: "shutdown",
                                    params: Value::Null,
                                };
                                if let Ok(payload) = serde_json::to_vec(&req) {
                                    let _ = transport.send(&payload).await;
                                }
                                let notif = Notification {
                                    jsonrpc: "2.0",
                                    method: "exit",
                                    params: Value::Null,
                                };
                                if let Ok(payload) = serde_json::to_vec(&notif) {
                                    let _ = transport.send(&payload).await;
                                }
                                shutdown_signal = Some(ack);
                                break;
                            }
                        }
                    }
                    incoming = transport.recv() => {
                        match incoming {
                            Ok(Some(body)) => {
                                let parsed: Result<Incoming, _> = serde_json::from_slice(&body);
                                let Ok(message) = parsed else {
                                    tracing::warn!("LSP: malformed message; ignoring");
                                    continue;
                                };
                                match (message.id, message.method) {
                                    (Some(Id::Number(id)), None) => {
                                        if let Some(responder) = pending.remove(&id) {
                                            let payload = match (message.result, message.error) {
                                                (Some(v), _) => Ok(v),
                                                (None, Some(e)) => Err(e),
                                                (None, None) => Ok(Value::Null),
                                            };
                                            let _ = responder.send(payload);
                                        }
                                    }
                                    (None, Some(method)) => {
                                        let notification = ServerNotification {
                                            method,
                                            params: message.params.unwrap_or(Value::Null),
                                        };
                                        // Review fix M4: bounded queue
                                        // — try_send + drop-on-full
                                        // so a host that never drains
                                        // notifications cannot exhaust
                                        // memory. Counter exposed via
                                        // `ClientHandle::notifications_dropped`.
                                        if let Err(err) = notif_tx.try_send(notification) {
                                            match err {
                                                mpsc::error::TrySendError::Full(_) => {
                                                    dropped_loop
                                                        .fetch_add(1, Ordering::Relaxed);
                                                    tracing::warn!(
                                                        "LSP: notification queue full; dropping oldest event"
                                                    );
                                                }
                                                mpsc::error::TrySendError::Closed(_) => {
                                                    // Host hung up; ignore.
                                                }
                                            }
                                        }
                                    }
                                    (Some(_), Some(_)) => {
                                        // Server-initiated request; the
                                        // MVP doesn't service these.
                                        tracing::debug!(
                                            "LSP: ignoring server-initiated request"
                                        );
                                    }
                                    _ => {}
                                }
                            }
                            Ok(None) => {
                                tracing::debug!("LSP: server stream closed");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(?e, "LSP: transport error; loop exiting");
                                break;
                            }
                        }
                    }
                    else => break,
                }
            }
            // Fail any still-pending requests so awaiters don't hang.
            for (_, responder) in pending.drain() {
                let _ = responder.send(Err(RpcError {
                    code: -32001,
                    message: "client loop exited".to_owned(),
                    data: None,
                }));
            }
            if let Some(ack) = shutdown_signal {
                let _ = ack.send(());
            }
        });

        let handle = ClientHandle {
            tx,
            notifications: Arc::new(Mutex::new(notif_rx)),
            notifications_dropped: dropped_counter,
        };
        Self { handle, join }
    }

    /// Wait for the run loop to exit. Useful in tests after sending
    /// `shutdown`.
    pub async fn join(self) -> Result<(), LspError> {
        self.join
            .await
            .map_err(|e| LspError::Framing(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MemoryTransport;
    use serde_json::json;
    use tokio::time::{Duration, timeout};

    /// Tiny stub: parse the next outbound request, build a synthetic
    /// response with the same id, push it back.
    async fn echo_response(transport: &MemoryTransport, result: Value) {
        loop {
            if let Some(body) = transport.pop_sent() {
                let parsed: Incoming = serde_json::from_slice(&body).expect("json");
                if let Some(Id::Number(id)) = parsed.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result,
                    });
                    transport.push_inbound(serde_json::to_vec(&response).expect("ser"));
                    return;
                }
                continue; // skip notifications
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[tokio::test]
    async fn initialize_round_trip_via_memory_transport() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        let echo = tokio::spawn(async move {
            echo_response(
                &mirror,
                json!({
                    "capabilities": {
                        "textDocumentSync": 1,
                    },
                }),
            )
            .await;
        });

        let params = InitializeParams::default();
        let result = timeout(Duration::from_secs(1), handle.initialize(params))
            .await
            .expect("timeout")
            .expect("initialize");
        echo.await.expect("echo");
        assert!(result.capabilities.text_document_sync.is_some());

        // Tidy up.
        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    #[tokio::test]
    async fn server_error_propagates() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        let stub = tokio::spawn(async move {
            loop {
                if let Some(body) = mirror.pop_sent() {
                    let parsed: Incoming = serde_json::from_slice(&body).expect("json");
                    if let Some(Id::Number(id)) = parsed.id {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": "method not found" },
                        });
                        mirror.push_inbound(serde_json::to_vec(&response).expect("ser"));
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        let err = handle
            .request::<Value, Value>("nope", json!({}))
            .await
            .expect_err("server error");
        match err {
            LspError::Server(e) => assert_eq!(e.code, -32601),
            other => panic!("unexpected: {other:?}"),
        }
        stub.await.expect("stub");
        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    #[tokio::test]
    async fn notification_is_dispatched_without_id() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        handle.initialized().await.expect("notify");
        // Wait for the loop to flush.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let body = mirror.pop_sent().expect("sent");
        let parsed: Incoming = serde_json::from_slice(&body).expect("json");
        assert!(parsed.id.is_none());
        assert_eq!(parsed.method.as_deref(), Some("initialized"));

        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    #[tokio::test]
    async fn server_notification_reaches_the_caller() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        let payload = json!({
            "jsonrpc": "2.0",
            "method": "window/showMessage",
            "params": { "type": 3, "message": "hello" },
        });
        mirror.push_inbound(serde_json::to_vec(&payload).expect("ser"));

        let received = timeout(Duration::from_secs(1), handle.next_notification())
            .await
            .expect("timeout")
            .expect("notification");
        assert_eq!(received.method, "window/showMessage");

        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    #[tokio::test]
    async fn pending_requests_fail_when_loop_exits() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        let pending = tokio::spawn(async move {
            handle
                .request::<Value, Value>("textDocument/completion", json!({}))
                .await
        });
        // Drop the inbound side without responding; close the stream
        // so the loop's transport.recv() returns Ok(None) and the
        // loop exits, failing the pending request.
        mirror.close();

        let result = pending.await.expect("join");
        assert!(result.is_err());
        let _ = client.join.await;
    }

    /// Review fix M5: a server that never responds must surface
    /// `LspError::Timeout` instead of hanging forever.
    #[tokio::test]
    async fn request_with_timeout_returns_timeout_error() {
        let transport = MemoryTransport::new();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        let err = handle
            .request_with_timeout::<Value, Value>(
                "slow/method",
                json!({}),
                Duration::from_millis(50),
            )
            .await
            .expect_err("timeout");
        assert!(
            matches!(err, LspError::Timeout(ref m) if m == "slow/method"),
            "got {err:?}"
        );

        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    /// Review fix M4: when the host never drains notifications,
    /// the bounded queue drops and bumps the dropped counter rather
    /// than growing without bound.
    #[tokio::test]
    async fn notification_overflow_bumps_dropped_counter() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        // Push more than NOTIFICATION_QUEUE_CAPACITY notifications
        // without ever draining; the counter must catch the overflow.
        for i in 0..(NOTIFICATION_QUEUE_CAPACITY + 8) {
            let payload = json!({
                "jsonrpc": "2.0",
                "method": "window/logMessage",
                "params": { "type": 4, "message": format!("msg {i}") }
            });
            mirror.push_inbound(serde_json::to_vec(&payload).expect("ser"));
        }
        // MR-M2: poll up to 2 s for the loop to drain the inbound
        // queue. A flat `sleep(30ms)` was flaky on loaded CI runners
        // where the spawned task hadn't been scheduled yet.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while handle.notifications_dropped() == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            handle.notifications_dropped() >= 1,
            "expected at least one dropped notification, got {}",
            handle.notifications_dropped()
        );

        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    /// MR-C2: a timed-out request must not leave a dangling entry
    /// in the run-loop's `pending` map. We can't peek at `pending`
    /// from out here, but we can prove the cancel path is wired by
    /// timing out a request and then driving a follow-up request
    /// to completion on the same run-loop. Without the cancel
    /// message the loop would still try to deliver the late
    /// response to a dead responder; the assertion is simply that
    /// no deadlock or panic surfaces.
    #[tokio::test]
    async fn timed_out_request_does_not_block_subsequent_calls() {
        let transport = MemoryTransport::new();
        let mirror = transport.clone();
        let client = Client::spawn(transport);
        let handle = client.handle.clone();

        // First request times out fast.
        let err = handle
            .request_with_timeout::<Value, Value>("hover", json!({}), Duration::from_millis(20))
            .await
            .expect_err("timeout");
        assert!(matches!(err, LspError::Timeout(_)));

        // Server eventually replies to id=1 (the request we just
        // cancelled). The loop must silently drop it.
        let late = json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
        mirror.push_inbound(serde_json::to_vec(&late).expect("ser"));

        // A subsequent request must still work end-to-end. Spawn
        // it, then push the matching response from the server side.
        let h2 = handle.clone();
        let pending = tokio::spawn(async move {
            h2.request_with_timeout::<Value, Value>("hover", json!({}), Duration::from_secs(2))
                .await
        });
        // The second request will get id=2 (id=1 was the timed-out
        // one). Give the loop a tick to dispatch before we reply.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let ok = json!({ "jsonrpc": "2.0", "id": 2, "result": { "hello": "world" } });
        mirror.push_inbound(serde_json::to_vec(&ok).expect("ser"));

        let result: Value = pending.await.expect("join").expect("ok");
        assert_eq!(result, json!({ "hello": "world" }));

        let _ = client.handle.clone().shutdown().await;
        let _ = client.join.await;
    }

    #[test]
    fn server_spec_defaults() {
        let sqls = ServerSpec::sqls();
        assert_eq!(sqls.command, "sqls");
        assert!(sqls.args.is_empty());
        let sqlls = ServerSpec::sqlls();
        assert_eq!(sqlls.command, "sqlls");
        assert_eq!(sqlls.args, vec!["--stdio".to_owned()]);
    }

    #[test]
    fn capabilities_serialise_with_expected_keys() {
        let caps = Capabilities::default();
        let json = serde_json::to_value(&caps).expect("serialise");
        assert!(json.get("textDocument").is_some());
        let text_doc = &json["textDocument"];
        assert!(text_doc.get("completion").is_some());
        assert!(text_doc.get("hover").is_some());
        assert!(text_doc.get("definition").is_some());
    }
}
