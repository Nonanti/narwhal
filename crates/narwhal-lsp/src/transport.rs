//! Transport layer: Content-Length-prefixed JSON-RPC framing.
//!
//! The protocol wraps every JSON-RPC message in an HTTP-style header:
//!
//! ```text
//! Content-Length: 123\r\n
//! \r\n
//! { "jsonrpc": "2.0", ... }
//! ```
//!
//! Anything more exotic (`Content-Type`, multipart, etc.) is not
//! emitted by sqls / sqlls in practice, so we ignore those headers on
//! the read path.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use crate::client::LspError;

/// Boxed future returned by [`Transport::send`].
pub type SendFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = Result<(), LspError>> + Send + 'a>>;

/// Boxed future returned by [`Transport::recv`].
pub type RecvFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = Result<Option<Vec<u8>>, LspError>> + Send + 'a>>;

/// A bidirectional byte transport between the client and the server.
/// Implemented for the stdio of a child process and for the in-memory
/// [`MemoryTransport`] used by tests.
pub trait Transport: Send {
    /// Write a single framed JSON-RPC message.
    ///
    /// The lifetime annotation is explicit because `body` must
    /// outlive the returned future; clippy reads this as an elidable
    /// `&self`-only lifetime, so we silence the lint locally.
    #[allow(clippy::needless_lifetimes)]
    fn send<'a>(&'a mut self, body: &'a [u8]) -> SendFuture<'a>;

    /// Read the next framed JSON-RPC message body. Returns `Ok(None)`
    /// when the peer has closed the stream cleanly.
    fn recv(&mut self) -> RecvFuture<'_>;
}

/// stdio-backed transport for a real child language server.
pub struct StdioTransport {
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
}

impl StdioTransport {
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            reader: BufReader::new(stdout),
            writer: stdin,
        }
    }
}

impl Transport for StdioTransport {
    #[allow(clippy::needless_lifetimes)]
    fn send<'a>(&'a mut self, body: &'a [u8]) -> SendFuture<'a> {
        Box::pin(async move { write_framed(&mut self.writer, body).await })
    }

    fn recv(&mut self) -> RecvFuture<'_> {
        Box::pin(async move { read_framed(&mut self.reader).await })
    }
}

/// Encode `body` with the Content-Length header and flush.
async fn write_framed<W>(writer: &mut W, body: &[u8]) -> Result<(), LspError>
where
    W: AsyncWrite + Unpin,
{
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(LspError::Io)?;
    writer.write_all(body).await.map_err(LspError::Io)?;
    writer.flush().await.map_err(LspError::Io)?;
    Ok(())
}

/// Read one Content-Length-prefixed JSON-RPC message.
async fn read_framed<R>(reader: &mut R) -> Result<Option<Vec<u8>>, LspError>
where
    R: AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.map_err(LspError::Io)?;
        if n == 0 {
            // Peer hung up before completing a frame; signal EOF.
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // End of headers.
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            let parsed: usize = value
                .trim()
                .parse()
                .map_err(|e: std::num::ParseIntError| LspError::Framing(e.to_string()))?;
            content_length = Some(parsed);
        }
        // Other headers are quietly ignored — the spec allows servers
        // to emit Content-Type, but no one does in practice.
    }
    let length = content_length
        .ok_or_else(|| LspError::Framing("missing Content-Length header before body".to_owned()))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.map_err(LspError::Io)?;
    Ok(Some(body))
}

/// In-memory transport used by unit tests and the smoke-test fixture.
/// Reads and writes round-trip through a shared `VecDeque<Vec<u8>>` so
/// a stub server can be implemented as a few `enqueue_response` calls
/// without spawning a subprocess.
#[derive(Clone, Default)]
pub struct MemoryTransport {
    inner: Arc<Mutex<MemoryState>>,
}

#[derive(Default)]
struct MemoryState {
    /// Messages the client has sent that the test harness can pop.
    sent: VecDeque<Vec<u8>>,
    /// Messages the test harness has queued that the client will recv.
    inbound: VecDeque<Vec<u8>>,
    /// Wakers parked on a pending recv waiting for inbound bytes.
    wakers: Vec<Waker>,
    /// Set when the harness calls [`MemoryTransport::close`].
    closed: bool,
}

impl MemoryTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pop the next message the client sent (FIFO). Returns `None` if
    /// the queue is empty.
    pub fn pop_sent(&self) -> Option<Vec<u8>> {
        self.inner
            .lock()
            .expect("memory transport poisoned")
            .sent
            .pop_front()
    }

    /// Number of unread messages the client has sent. Useful in tests
    /// that wait for the request half of a round-trip.
    pub fn sent_len(&self) -> usize {
        self.inner
            .lock()
            .expect("memory transport poisoned")
            .sent
            .len()
    }

    /// Enqueue a server-originated message the client will receive.
    pub fn push_inbound(&self, body: Vec<u8>) {
        let wakers = {
            let mut state = self.inner.lock().expect("memory transport poisoned");
            state.inbound.push_back(body);
            std::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Signal end-of-stream so a pending `recv` returns `Ok(None)`.
    pub fn close(&self) {
        let wakers = {
            let mut state = self.inner.lock().expect("memory transport poisoned");
            state.closed = true;
            std::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Resolve a pending recv with the next inbound message, parking
    /// the waker if the queue is empty.
    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<Option<Vec<u8>>, LspError>> {
        let mut state = self.inner.lock().expect("memory transport poisoned");
        if let Some(body) = state.inbound.pop_front() {
            Poll::Ready(Ok(Some(body)))
        } else if state.closed {
            Poll::Ready(Ok(None))
        } else {
            state.wakers.push(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Transport for MemoryTransport {
    #[allow(clippy::needless_lifetimes)]
    fn send<'a>(&'a mut self, body: &'a [u8]) -> SendFuture<'a> {
        let owned = body.to_vec();
        let inner = self.inner.clone();
        Box::pin(async move {
            let mut state = inner.lock().expect("memory transport poisoned");
            state.sent.push_back(owned);
            Ok(())
        })
    }

    fn recv(&mut self) -> RecvFuture<'_> {
        let this = self.clone();
        Box::pin(std::future::poll_fn(move |cx| this.poll_recv(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn frames_round_trip_via_in_memory_pipe() {
        let (mut tx, rx) = tokio::io::duplex(4096);
        write_framed(&mut tx, br#"{"hello":"world"}"#)
            .await
            .unwrap();
        let mut reader = BufReader::new(rx);
        let body = read_framed(&mut reader).await.unwrap().expect("frame");
        assert_eq!(body, br#"{"hello":"world"}"#);
    }

    #[tokio::test]
    async fn read_frame_returns_eof_on_clean_close() {
        let (tx, rx) = tokio::io::duplex(4096);
        drop(tx);
        let mut reader = BufReader::new(rx);
        let body = read_framed(&mut reader).await.unwrap();
        assert!(body.is_none());
    }

    #[tokio::test]
    async fn missing_content_length_is_a_framing_error() {
        let (mut tx, rx) = tokio::io::duplex(4096);
        let _ = tx.write_all(b"\r\n").await;
        drop(tx);
        let mut reader = BufReader::new(rx);
        let err = read_framed(&mut reader).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Content-Length"), "got: {msg}");
    }

    #[tokio::test]
    async fn ignores_unknown_headers() {
        let (mut tx, rx) = tokio::io::duplex(4096);
        let _ = tx
            .write_all(b"Content-Type: application/json\r\nContent-Length: 2\r\n\r\nhi")
            .await;
        drop(tx);
        let mut reader = BufReader::new(rx);
        let body = read_framed(&mut reader).await.unwrap().unwrap();
        assert_eq!(body, b"hi");
    }

    #[tokio::test]
    async fn memory_transport_round_trips_messages() {
        let mut transport = MemoryTransport::new();
        let body = br#"{"ping":true}"#;
        transport.send(body).await.unwrap();
        let popped = transport.pop_sent().expect("sent");
        assert_eq!(popped, body);
        transport.push_inbound(br#"{"pong":true}"#.to_vec());
        let received = transport.recv().await.unwrap().expect("frame");
        assert_eq!(received, br#"{"pong":true}"#);
    }

    #[tokio::test]
    async fn memory_transport_close_resolves_pending_recv() {
        let mut transport = MemoryTransport::new();
        let transport_clone = transport.clone();
        let recv_fut = transport.recv();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            transport_clone.close();
        });
        let result = recv_fut.await.unwrap();
        assert!(result.is_none());
    }
}
