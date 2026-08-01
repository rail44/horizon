//! The daemon-side lifecycle every runtime process repeats verbatim: bind
//! the socket, accept one connection at a time, serve a hub over remoc,
//! unlink on the way out. [`crate::spawn`] is the client's half of the same
//! seam; [`crate::socket`] says where the path is.
//!
//! Domain-free, like the rest of this crate: the daemon's *name* is a
//! parameter (it only ever reaches a log line or an error message), the hub
//! it serves is a type parameter, and the two places the runtimes genuinely
//! differ are explicit hooks rather than branches --
//!
//! - **what SIGTERM must do first** ([`run`]'s `on_sigterm`): agentd flushes
//!   its event log, terminald kills every PTY. That difference *is* the
//!   terminald split (`docs/terminald-split-design.md`) and must never be
//!   unified into a shared "shutdown";
//! - **what a lost connection must clean up** ([`serve_connection`]'s
//!   `on_disconnect`): the sessions themselves are process-scoped in both
//!   daemons and keep running -- only their bridges to the departed client
//!   die.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use remoc::rtc::{self, Client as _};
use remoc::RemoteSend;
use tokio::net::{UnixListener, UnixStream};

use crate::channels::{RTC_MAX_REPLY_BYTES, RTC_MAX_REQUEST_BYTES};
use crate::codec::WireCodec;

/// How long an accepted connection gets to complete the remoc (chmux)
/// handshake before the daemon gives up on it. A current-generation client
/// completes it in milliseconds; what this bounds is a peer that is not
/// speaking chmux at all -- a JSONL-generation (v<=9) client, whose
/// line-framed hello is chmux garbage, or a port scanner -- neither of
/// which may wedge the one-at-a-time accept loop for chmux's raw 60 s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many in-flight rtc requests a hub server buffers.
const REQUEST_BUFFER: usize = 16;

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// instance that didn't shut down cleanly), remove it and rebind. If
/// something *is* accepting, refuses to steal the path out from under a live
/// instance -- which is what keeps a racing second spawn from orphaning what
/// the first one already owns (terminald's PTYs above all: they outlive
/// every connection, so a second daemon on the same path would strand them
/// with no client able to find them again).
///
/// `daemon_name` names the binary in both messages, so the diagnostics read
/// the same as when each daemon carried its own copy of this function.
pub async fn bind_listener(path: &Path, daemon_name: &str) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another {daemon_name} running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "{daemon_name}: removing stale socket {} (nothing was accepting)",
                    path.display()
                );
                std::fs::remove_file(path)?;
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(UnixListener::bind(path)?)
}

/// The `--socket <path>` / `--socket=<path>` override every daemon accepts,
/// which always wins over [`crate::socket`]'s default resolution.
pub fn socket_path_from_args<I: Iterator<Item = String>>(mut args: I) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--socket=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

/// The accept loop: serve connections one at a time until SIGTERM, then run
/// `on_sigterm` and unlink the socket.
///
/// `on_sigterm` is the one thing that is genuinely per-daemon here (see the
/// module doc) and it runs *before* the socket is unlinked, so a client that
/// still holds a connection cannot observe the daemon as gone before its
/// shutdown obligation has been met.
pub async fn run<Fut>(
    listener: UnixListener,
    socket_path: &Path,
    daemon_name: &str,
    mut serve: impl FnMut(UnixStream) -> Fut,
    on_sigterm: impl FnOnce(),
) -> anyhow::Result<()>
where
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = serve(stream).await {
                    eprintln!("{daemon_name}: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("{daemon_name}: SIGTERM received, shutting down");
                on_sigterm();
                break;
            }
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// Serves one hub over one accepted connection, to completion: establish the
/// remoc connection (bounded by [`CONNECT_TIMEOUT`] so a peer that never
/// speaks chmux cannot wedge [`run`]'s accept loop), hand the client its
/// generated hub client over the base channel, then serve `target` with
/// per-call task spawning until the client goes away.
///
/// One connection at a time by construction: [`run`] awaits this before
/// accepting the next connection (multi-client support is explicitly out of
/// scope -- see `docs/agent-runtime-split-design.md`'s "Out of scope here").
/// Version negotiation is *not* a transport-level concern: it is the `hello`
/// rtc call, answered by the hub itself (`docs/remoc-adoption-design.md`
/// §3), which is also why a range-rejected client can still call `drain` --
/// the auto-recovery path that restarts a stale daemon at the right version.
///
/// A dropped connection is never a session lifecycle event: sessions are
/// scoped to the process, not the connection, so `on_disconnect` tears down
/// only this connection's bridges to them.
pub async fn serve_connection<Target, Server>(
    stream: UnixStream,
    daemon_name: &str,
    target: Arc<Target>,
    on_disconnect: impl FnOnce(),
) -> anyhow::Result<()>
where
    Server: rtc::ServerShared<Target, WireCodec>,
    Server::Client: RemoteSend + Clone,
{
    let (read_half, write_half) = stream.into_split();
    let connect = remoc::Connect::io::<_, _, Server::Client, (), WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, mut base_tx, _base_rx) = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => {
            eprintln!(
                "{daemon_name}: dropping a connection that failed the remoc handshake \
                 (a pre-v10 JSONL client, or not a Horizon client at all): {error}"
            );
            return Ok(());
        }
        Err(_elapsed) => {
            eprintln!(
                "{daemon_name}: dropping a connection with no remoc handshake within \
                 {CONNECT_TIMEOUT:?} (a pre-v10 JSONL client, or not a Horizon client at all)"
            );
            return Ok(());
        }
    };
    // The chmux multiplexer must be polled for the connection to make any
    // progress (adoption condition 3) -- spawned, so it runs alongside the
    // serve loop below.
    let mut conn_task = tokio::spawn(conn);

    let (server, mut client) = Server::new(target, REQUEST_BUFFER);
    // Effective placement of the rtc size caps (see the `RTC_MAX_*`
    // constants): the request cap must be set on the client *before* it is
    // transported -- a transported mpsc sender carries its creator's cap as
    // the daemon-side receive limit, so an oversized request is dropped
    // per-item here and the call fails client-side when its reply channel
    // closes. The reply cap travels per-request from the client's own
    // setting; this one seeds the default.
    client.set_max_request_size(RTC_MAX_REQUEST_BYTES);
    client.set_max_reply_size(RTC_MAX_REPLY_BYTES);
    if base_tx.send(client).await.is_err() {
        conn_task.abort();
        return Ok(());
    }

    // `serve` ends when the client (and every clone of it) is gone --
    // dropped by the UI, or severed with the connection. Racing the mux
    // task covers the pathological case where the mux dies without the
    // serve loop noticing.
    tokio::select! {
        served = server.serve(true) => {
            if let Err(error) = served {
                eprintln!("{daemon_name}: hub serve error: {error}");
            }
        }
        _ = &mut conn_task => {}
    }

    on_disconnect();
    conn_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_from_args_reads_the_space_separated_form() {
        let args = vec!["--socket".to_string(), "/tmp/sock".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock"))
        );
    }

    #[test]
    fn socket_path_from_args_reads_the_equals_form() {
        let args = vec!["--socket=/tmp/sock2".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock2"))
        );
    }

    #[test]
    fn socket_path_from_args_is_none_when_the_flag_is_absent() {
        let args: Vec<String> = vec!["--other-flag".to_string()];
        assert_eq!(socket_path_from_args(args.into_iter()), None);
    }
}
