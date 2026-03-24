//! SSH local-port-forward tunnel powered by the system `ssh` binary.
//!
//! We deliberately shell out to OpenSSH rather than embedding an SSH
//! library so users get the full ecosystem (`~/.ssh/config`,
//! `ssh-agent`, `IdentityAgent`, `Match` blocks, jump hosts, FIDO2
//! keys, …) for free. The compile-time cost would otherwise be enormous
//! for a tunnel that's only configured by a tiny fraction of users.
//!
//! A [`SshTunnel`] is alive for as long as the value is in scope: its
//! `Drop` impl sends `SIGTERM` to the spawned subprocess so the
//! forwarded port goes away with the database session.
//!
//! # Wire-up
//!
//! ```ignore
//! let tunnel = SshTunnel::spawn(&ssh_config, "db.internal", 5432)?;
//! let host = tunnel.local_host();
//! let port = tunnel.local_port();
//! // … hand `host`/`port` to the driver instead of the original ones.
//! ```
//!
//! The function blocks until either:
//! - the subprocess exits (failure), or
//! - the forwarded port accepts a TCP connection (success).
//!
//! Either way it returns within `Self::READY_TIMEOUT`.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::connection::SshConfig;

/// Maximum time we wait for the forwarded port to become reachable
/// before giving up. Tuned to be long enough for slow VPN handshakes
/// without making the connect path hang noticeably on failure.
pub const READY_TIMEOUT: Duration = Duration::from_secs(8);

/// Handle to a running `ssh -L` subprocess. Dropping the value tears
/// the tunnel down.
#[derive(Debug)]
pub struct SshTunnel {
    child: Child,
    local_port: u16,
    target: String,
}

impl SshTunnel {
    /// Spawn an `ssh -L 127.0.0.1:<picked>:<target_host>:<target_port>`
    /// subprocess and wait for the forwarded port to start accepting
    /// connections.
    ///
    /// Errors surface as [`io::Error`] with `ErrorKind::Other` so the
    /// caller can wrap them in the driver-agnostic
    /// [`crate::Error::Connection`] variant without losing the
    /// original message.
    /// Async wrapper around [`Self::spawn`] suitable for the tokio
    /// runtime.
    ///
    /// M2: the legacy `spawn` method internally `std::thread::sleep`s
    /// for up to [`READY_TIMEOUT`] (8s) while polling the forwarded
    /// port. Calling that directly from an async task blocks the
    /// scheduler thread — if the runtime is `current_thread`, the
    /// whole UI freezes for the duration. Hopping onto a blocking
    /// thread via `spawn_blocking` keeps the runtime responsive while
    /// preserving the synchronous OS-process semantics that the rest
    /// of the impl depends on.
    ///
    /// Prefer this over `spawn` in async code paths; the sync version
    /// stays public for tests and the rare CLI-only entry point.
    pub async fn spawn_async(
        config: SshConfig,
        target_host: String,
        target_port: u16,
    ) -> io::Result<Self> {
        tokio::task::spawn_blocking(move || Self::spawn(&config, &target_host, target_port))
            .await
            .map_err(|e| io::Error::other(format!("ssh tunnel spawn task panicked: {e}")))?
    }

    pub fn spawn(config: &SshConfig, target_host: &str, target_port: u16) -> io::Result<Self> {
        let local_port = pick_free_port()?;
        let target = format!("{target_host}:{target_port}");
        let bind_spec = format!("127.0.0.1:{local_port}:{target}");

        let mut cmd = Command::new("ssh");
        cmd.arg("-N") // No remote command, port forwarding only.
            .arg("-T") // Disable PTY allocation (we never type into ssh).
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-L")
            .arg(&bind_spec);
        if let Some(port) = config.port {
            cmd.arg("-p").arg(port.to_string());
        }
        if let Some(key) = config.key_path.as_ref() {
            cmd.arg("-i").arg(key);
        }
        if let Some(jump) = config.jump_host.as_ref() {
            cmd.arg("-J").arg(jump);
        }
        let user_at_host = format!("{}@{}", config.user, config.host);
        cmd.arg(&user_at_host);

        // Inherit nothing on stdin so a missing agent doesn't make the
        // child wait on a password prompt forever. stderr is piped so
        // we can include it in the error message on failure.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("could not spawn ssh: {e} (is the OpenSSH client installed?)"),
            )
        })?;

        let mut tunnel = Self {
            child,
            local_port,
            target: user_at_host,
        };
        tunnel.wait_for_ready()?;
        Ok(tunnel)
    }

    /// Loopback host the driver should connect to.
    pub const fn local_host(&self) -> &'static str {
        "127.0.0.1"
    }

    pub const fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Polls the forwarded port until it accepts a TCP connection, the
    /// subprocess exits, or [`READY_TIMEOUT`] elapses — whichever
    /// happens first.
    ///
    /// A dead subprocess is detected within ~100 ms via `try_wait`, so
    /// a missing binary / immediate ssh error does not stall the connect
    /// path for the full `READY_TIMEOUT`. When the child has exited
    /// before the port is up we surface its captured stderr.
    fn wait_for_ready(&mut self) -> io::Result<()> {
        // `127.0.0.1:<port>` with a `u16` port is always a syntactically
        // valid SocketAddr; the parse can only fail for malformed input,
        // which the format string makes impossible.
        let addr: SocketAddr = format!("127.0.0.1:{}", self.local_port)
            .parse()
            .expect("127.0.0.1:<u16> is always a valid SocketAddr");
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
                // Drop the probe socket immediately; we just wanted the
                // handshake confirmation.
                drop(stream);
                return Ok(());
            }
            // Subprocess died before the port came up — read its
            // stderr and surface the underlying ssh diagnostic rather
            // than waiting out the timeout.
            if let Ok(Some(status)) = self.child.try_wait() {
                let stderr = self.drain_stderr();
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!(
                        "ssh tunnel to {} exited ({status}) before the port was ready: {}",
                        self.target,
                        stderr.trim()
                    ),
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "ssh tunnel to {} did not accept connections within {:?}",
                        self.target, READY_TIMEOUT
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Best-effort drain of the child's stderr pipe. Returns an empty
    /// string when stderr is missing or unreadable so the caller can
    /// always interpolate it without an extra Option dance.
    fn drain_stderr(&mut self) -> String {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut err) = self.child.stderr.take() {
            let _ = err.read_to_string(&mut buf);
        }
        buf
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        // Best-effort SIGTERM. The kernel reclaims the port even if
        // the child happens to be wedged.
        let _ = self.child.kill();
        // Reap so we don't leave a zombie behind. Ignore the result;
        // the connection is going away regardless.
        let _ = self.child.wait();
    }
}

/// Ask the kernel for a free loopback port by binding to port 0 and
/// then dropping the listener. The race window between drop and the
/// ssh subprocess binding the same port is small enough that we
/// accept it; if it ever bites we'll move to a retry loop.
fn pick_free_port() -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity-check the port picker actually hands out a usable port.
    /// We bind it ourselves to prove the kernel didn't lie about it
    /// being free.
    #[test]
    fn pick_free_port_yields_bindable_port() {
        let port = pick_free_port().unwrap();
        let _l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    /// M2: `spawn_async` must complete without blocking the runtime
    /// thread — prove it by running it inside a `current_thread`
    /// runtime with a single worker and a separately-spawned
    /// concurrent task that bumps a counter while the tunnel is
    /// being set up. If the runtime were blocked, the counter would
    /// stall.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_async_does_not_block_current_thread_runtime() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        let s = Arc::clone(&stop);
        let bumper = tokio::spawn(async move {
            while s.load(Ordering::Relaxed) == 0 {
                c.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        });

        // Hits the failure path (TEST-NET-1 unroutable address).
        let cfg = SshConfig::new("192.0.2.1", "nobody");
        let outcome = SshTunnel::spawn_async(cfg, "127.0.0.1".into(), 1).await;
        assert!(outcome.is_err(), "expected failure, got: {outcome:?}");

        stop.store(1, Ordering::Relaxed);
        let _ = bumper.await;
        let n = counter.load(Ordering::Relaxed);
        // If the runtime had been blocked for ~8s the bumper would
        // have made zero progress. A few hundred increments is the
        // worst-case for a heavily loaded CI box; we just need to
        // see *some* concurrent progress.
        assert!(n > 10, "expected concurrent progress, got {n} increments");
    }

    /// Spawning against a deliberately invalid host fails within the
    /// deadline rather than hanging the test runner. This guards the
    /// connect path's timeout — we can't easily spin up a real sshd
    /// in CI but we can at least prove the failure path terminates.
    #[test]
    fn spawn_fails_fast_against_unreachable_host() {
        // Use a TEST-NET-1 address (RFC 5737) so we don't accidentally
        // hit a real server. ssh will fail to resolve/connect quickly.
        let cfg = SshConfig::new("192.0.2.1", "nobody");
        let start = Instant::now();
        let outcome = SshTunnel::spawn(&cfg, "127.0.0.1", 1);
        let elapsed = start.elapsed();
        assert!(outcome.is_err(), "expected failure, got: {outcome:?}");
        assert!(
            elapsed <= READY_TIMEOUT + Duration::from_secs(2),
            "spawn took {elapsed:?}, expected <= {READY_TIMEOUT:?}"
        );
    }
}
