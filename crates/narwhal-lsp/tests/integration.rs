//! Integration tests for M5.2: LSP client over `MemoryTransport`.
//!
//! Scenarios:
//! 1. Initialize handshake
//! 2. `textDocument/completion` → `CompletionResponse::Array`
//! 3. `textDocument/completion` → `CompletionResponse::List`
//! 4. `textDocument/hover` → `Hover`
//! 5. Notification fan-out: server `window/showMessage` → client `next_notification()`
//! 6. Timeout + cancel: `request_with_timeout` timeout → cancel notification, stale response ignored
//! 7. Dropped notification counter: >256 notifications → counter increments
//! 8. Shutdown clean

use std::time::Duration;

use narwhal_lsp::{Client, MemoryTransport, jsonrpc::Id, lsp_types};
use serde_json::{Value, json};
use tokio::time::timeout;

/// Helper: pop the next outbound request from the transport and push
/// a JSON-RPC response with the same id and the given `result` value.
async fn respond(transport: &MemoryTransport, result: Value) {
    let deadline = Duration::from_millis(500);
    let start = std::time::Instant::now();
    loop {
        if let Some(body) = transport.pop_sent() {
            let Ok(parsed) = serde_json::from_slice::<narwhal_lsp::jsonrpc::Incoming>(&body) else {
                continue;
            };
            if let Some(Id::Number(id)) = parsed.id {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                });
                transport.push_inbound(serde_json::to_vec(&response).expect("ser"));
                return;
            }
            // skip notifications — they have no id
            continue;
        }
        assert!(
            start.elapsed() <= deadline,
            "timed out waiting for outbound request"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

// ---- Test 1: Initialize handshake ----

#[tokio::test]
async fn initialize_handshake() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    let responder = tokio::spawn(async move {
        respond(
            &mirror,
            json!({
                "capabilities": {
                    "textDocumentSync": 1,
                    "completionProvider": {},
                    "hoverProvider": true,
                },
            }),
        )
        .await;
    });

    let params = lsp_types::InitializeParams::default();
    let result = timeout(Duration::from_secs(2), handle.initialize(params))
        .await
        .expect("timeout")
        .expect("initialize");

    assert!(result.capabilities.completion_provider.is_some());
    assert!(result.capabilities.hover_provider.is_some());

    responder.await.expect("responder");

    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 2: completion returning CompletionResponse::Array ----

#[tokio::test]
async fn completion_returns_array() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    let responder = tokio::spawn(async move {
        respond(
            &mirror,
            json!([
                { "label": "SELECT", "kind": 14 },
                { "label": "SET", "kind": 14 },
            ]),
        )
        .await;
    });

    // First handle the initialize request that the client might send.
    // Actually, we need to call completion directly.
    let params = lsp_types::CompletionParams {
        text_document_position: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: lsp_types::Url::parse("file:///test.sql").unwrap(),
            },
            position: lsp_types::Position::new(0, 0),
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams {
            work_done_token: None,
        },
        partial_result_params: lsp_types::PartialResultParams {
            partial_result_token: None,
        },
        context: None,
    };

    let result = timeout(Duration::from_secs(2), handle.completion(params))
        .await
        .expect("timeout")
        .expect("completion");

    match result {
        Some(lsp_types::CompletionResponse::Array(items)) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].label, "SELECT");
            assert_eq!(items[1].label, "SET");
        }
        other => panic!("expected Array, got {other:?}"),
    }

    responder.await.expect("responder");
    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 3: completion returning CompletionResponse::List ----

#[tokio::test]
async fn completion_returns_list() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    let responder = tokio::spawn(async move {
        respond(
            &mirror,
            json!({
                "isIncomplete": false,
                "items": [
                    { "label": "SELECT", "kind": 14 },
                    { "label": "INSERT", "kind": 14 },
                ],
            }),
        )
        .await;
    });

    let params = lsp_types::CompletionParams {
        text_document_position: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: lsp_types::Url::parse("file:///test.sql").unwrap(),
            },
            position: lsp_types::Position::new(0, 0),
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams {
            work_done_token: None,
        },
        partial_result_params: lsp_types::PartialResultParams {
            partial_result_token: None,
        },
        context: None,
    };

    let result = timeout(Duration::from_secs(2), handle.completion(params))
        .await
        .expect("timeout")
        .expect("completion");

    match result {
        Some(lsp_types::CompletionResponse::List(list)) => {
            assert!(!list.is_incomplete);
            assert_eq!(list.items.len(), 2);
            assert_eq!(list.items[0].label, "SELECT");
            assert_eq!(list.items[1].label, "INSERT");
        }
        other => panic!("expected List, got {other:?}"),
    }

    responder.await.expect("responder");
    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 4: hover ----

#[tokio::test]
async fn hover_returns_result() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    let responder = tokio::spawn(async move {
        respond(
            &mirror,
            json!({
                "contents": {
                    "kind": "plaintext",
                    "value": "TABLE users",
                },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 5 },
                },
            }),
        )
        .await;
    });

    let params = lsp_types::HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: lsp_types::Url::parse("file:///test.sql").unwrap(),
            },
            position: lsp_types::Position::new(0, 0),
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams {
            work_done_token: None,
        },
    };

    let result = timeout(Duration::from_secs(2), handle.hover(params))
        .await
        .expect("timeout")
        .expect("hover");

    match result {
        Some(hover) => {
            match hover.contents {
                lsp_types::HoverContents::Markup(markup) => {
                    assert_eq!(markup.kind, lsp_types::MarkupKind::PlainText);
                    assert_eq!(markup.value, "TABLE users");
                }
                other => panic!("expected Markup contents, got {other:?}"),
            }
            assert!(hover.range.is_some());
        }
        None => panic!("expected Some(Hover), got None"),
    }

    responder.await.expect("responder");
    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 5: notification fan-out ----

#[tokio::test]
async fn server_notification_reaches_client() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    let payload = json!({
        "jsonrpc": "2.0",
        "method": "window/showMessage",
        "params": { "type": 3, "message": "hello from server" },
    });
    mirror.push_inbound(serde_json::to_vec(&payload).expect("ser"));

    let received = timeout(Duration::from_secs(2), handle.next_notification())
        .await
        .expect("timeout")
        .expect("notification");

    assert_eq!(received.method, "window/showMessage");
    assert_eq!(received.params["message"], json!("hello from server"));

    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 6: timeout with cancel ----

#[tokio::test]
async fn request_with_timeout_cancels_on_timeout() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    // Issue a request with a very short timeout — no server response
    // will arrive in time.
    let err = handle
        .request_with_timeout::<Value, Value>(
            "textDocument/completion",
            json!({}),
            Duration::from_millis(30),
        )
        .await
        .expect_err("should timeout");

    match err {
        narwhal_lsp::LspError::Timeout(method) => {
            assert_eq!(method, "textDocument/completion");
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    // A subsequent request must still work (cancel path cleaned up).
    // We need to drain any stale outbound request from the first
    // timed-out call, then respond to the second request.
    let h2 = handle.clone();
    let m2 = mirror.clone();
    let responder = tokio::spawn(async move {
        // Respond to ALL outbound requests we see — the first will
        // be the cancelled one (its responder is gone so the run-loop
        // drops the response); the second will be our real request.
        let mut responded = 0usize;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while responded < 2 && std::time::Instant::now() < deadline {
            if let Some(body) = m2.pop_sent() {
                let Ok(parsed) = serde_json::from_slice::<narwhal_lsp::jsonrpc::Incoming>(&body)
                else {
                    continue;
                };
                if let Some(Id::Number(id)) = parsed.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "hello": "world" },
                    });
                    m2.push_inbound(serde_json::to_vec(&response).expect("ser"));
                    responded += 1;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });

    let result: Value = timeout(
        Duration::from_secs(2),
        h2.request_with_timeout("textDocument/hover", json!({}), Duration::from_secs(2)),
    )
    .await
    .expect("timeout2")
    .expect("hover request");
    assert_eq!(result["hello"], "world");

    responder.await.expect("responder");
    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 7: dropped notification counter ----

#[tokio::test]
async fn notification_overflow_increments_dropped_counter() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    // Push more than 256 notifications without draining them.
    let capacity = 256;
    for i in 0..(capacity + 10) {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": { "type": 4, "message": format!("msg {i}") },
        });
        mirror.push_inbound(serde_json::to_vec(&payload).expect("ser"));
    }

    // Poll until the dropped counter goes above zero or we time out.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while handle.notifications_dropped() == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        handle.notifications_dropped() >= 1,
        "expected at least one dropped notification, got {}",
        handle.notifications_dropped()
    );

    let _ = handle.shutdown().await;
    let _ = client.join().await;
}

// ---- Test 8: shutdown clean ----

#[tokio::test]
async fn shutdown_is_clean() {
    let transport = MemoryTransport::new();
    let mirror = transport.clone();
    let client = Client::spawn(transport);
    let handle = client.handle.clone();

    // Respond to the shutdown request that `shutdown()` sends.
    let responder = tokio::spawn(async move {
        // The shutdown method sends a request then an exit notification.
        // Pop and respond to the request.
        let deadline = Duration::from_secs(2);
        let start = std::time::Instant::now();
        loop {
            if let Some(body) = mirror.pop_sent() {
                let Ok(parsed) = serde_json::from_slice::<narwhal_lsp::jsonrpc::Incoming>(&body)
                else {
                    continue;
                };
                if let Some(Id::Number(id)) = parsed.id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": null,
                    });
                    mirror.push_inbound(serde_json::to_vec(&response).expect("ser"));
                    return;
                }
            }
            assert!(
                start.elapsed() <= deadline,
                "timed out waiting for shutdown request"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    handle.shutdown().await.expect("shutdown");
    client.join().await.expect("join");
    responder.await.expect("responder");
}
