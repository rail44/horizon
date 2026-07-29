//! `horizon-terminald`: the terminal daemon
//! (`docs/terminald-split-design.md`). Owns every PTY and the terminal
//! emulation loops (`horizon-terminal-core`), served over its own unix
//! socket as the [`TerminalHub`] rtc trait — a sibling of, and completely
//! independent from, `horizon-sessiond`'s agent hub.
//!
//! **Why this is a separate process.** `Reload Session Runtime` restarts the
//! agent runtime constantly (an agent-side rebuild, a `[provider]` change);
//! before this split it also killed every PTY, taking the interactive CLIs
//! running in them down with it, because one process hosted both. Two thirds
//! of the daemon commits in the measured window touched only the agent side
//! and had no causal need to restart a terminal. So terminals moved out, and
//! this daemon is deliberately *rarely* restarted: `Reload Terminal Runtime`
//! is the only command that drains it, and it is explicitly destructive.
//!
//! That longevity is also why the terminal wire slice is append-only from
//! v17 on (design decision 5, see [`TerminalHub`]'s doc): a running
//! terminald may be an older binary than the UI that connects to it, and the
//! only honest response to a below-the-schema mismatch is a clean refusal
//! naming `Reload Terminal Runtime` — never silent misbehavior (the tmux 3.6
//! lesson; decision 6, implemented client-side in `src/sessiond/`).
//!
//! **No persistence, no readiness gate.** Unlike sessiond, this daemon owns
//! no event log and no DuckDB projection, so there is nothing to resume at
//! startup and nothing to flush on drain: bind, accept, serve. A terminal
//! session's whole state lives in its PTY and its emulator, and the
//! backstop for losing this process is the workspace snapshot restore,
//! exactly as before.
//!
//! **Dependency-graph note.** This crate depends on
//! `horizon-session-protocol`, which names the agent vocabulary too, so
//! `horizon-agent` (and thus DuckDB) is in this binary's link graph even
//! though not one symbol of it is used here. That is a build-time artifact
//! only — the runtime independence this split exists for is process,
//! socket, and hub-trait separation, all of which hold. Splitting the
//! protocol crate's domain-free foundation into its own crate would remove
//! it; that is tracked as follow-up rather than done here, so the wire
//! itself moves exactly once (`docs/tasks/backlog.md`).

mod hub;
mod terminal;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::socket::default_terminald_socket_path;
use horizon_session_protocol::{
    TerminalHubClient, TerminalHubServerShared, WireCodec, RTC_MAX_REPLY_BYTES,
    RTC_MAX_REQUEST_BYTES,
};
use hub::Hub;
use remoc::rtc::{Client as _, ServerShared as _};
use terminal::TerminalHost;
use tokio::net::{UnixListener, UnixStream};

/// Reported in this binary's `hello` reply's `binary_id` — the value the
/// client records for decision 6's skew message. The negotiated protocol
/// version is carried separately in the same `TerminalHubHello`.
const BINARY_ID: &str = concat!("horizon-terminald/", env!("CARGO_PKG_VERSION"));

/// How long an accepted connection gets to complete the remoc (chmux)
/// handshake before the daemon gives up on it — the same bound sessiond
/// applies, for the same reason: a peer that is not speaking chmux (a port
/// scanner, a stale generation) must not wedge the one-at-a-time accept
/// loop for chmux's raw 60 s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_terminald_socket_path);
    eprintln!("horizon-terminald: starting on {}", socket_path.display());

    let listener = bind_listener(&socket_path).await?;
    let terminals = TerminalHost::new();
    run(listener, &socket_path, terminals).await
}

async fn run(
    listener: UnixListener,
    socket_path: &Path,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = handle_connection(stream, terminals.clone()).await {
                    eprintln!("horizon-terminald: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-terminald: SIGTERM received, shutting down");
                terminals.shutdown_all();
                break;
            }
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// One connection at a time by construction: [`run`]'s accept loop awaits
/// this to completion before accepting the next connection (multi-client
/// support is explicitly out of scope, exactly as for sessiond). Establish
/// the remoc connection (bounded by [`CONNECT_TIMEOUT`]), hand the client
/// its `TerminalHubClient` over the base channel, then serve the [`Hub`]
/// with per-call task spawning until the client goes away.
///
/// A dropped connection is *not* a session lifecycle event: the terminals
/// keep running (process-scoped), which is the whole point — a UI restart,
/// or a `Reload Session Runtime` that happens to reconnect everything, finds
/// them alive and re-attaches.
async fn handle_connection(stream: UnixStream, terminals: TerminalHost) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let connect = remoc::Connect::io::<_, _, TerminalHubClient<WireCodec>, (), WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, mut base_tx, _base_rx) = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => {
            eprintln!(
                "horizon-terminald: dropping a connection that failed the remoc handshake \
                 (not a Horizon client, or one built against a different transport): {error}"
            );
            return Ok(());
        }
        Err(_elapsed) => {
            eprintln!(
                "horizon-terminald: dropping a connection with no remoc handshake within \
                 {CONNECT_TIMEOUT:?}"
            );
            return Ok(());
        }
    };
    // The chmux multiplexer must be polled for the connection to make any
    // progress (adoption condition 3) -- spawned, so it runs alongside the
    // serve loop below.
    let mut conn_task = tokio::spawn(conn);

    let hub = Hub::new(terminals.clone(), BINARY_ID);
    let (server, mut client) = TerminalHubServerShared::<_, WireCodec>::new(Arc::new(hub), 16);
    // Effective placement of the rtc size caps (see the `RTC_MAX_*`
    // constants): the request cap must be set on the client *before* it is
    // transported -- a transported mpsc sender carries its creator's cap as
    // the daemon-side receive limit.
    client.set_max_request_size(RTC_MAX_REQUEST_BYTES);
    client.set_max_reply_size(RTC_MAX_REPLY_BYTES);
    if base_tx.send(client).await.is_err() {
        conn_task.abort();
        return Ok(());
    }

    tokio::select! {
        served = server.serve(true) => {
            if let Err(error) = served {
                eprintln!("horizon-terminald: hub serve error: {error}");
            }
        }
        _ = &mut conn_task => {}
    }

    // Post-connection cleanup: sessions keep running, but their bridges to
    // this connection are dead.
    terminals.clear_subscribers();
    conn_task.abort();
    Ok(())
}

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// `horizon-terminald` that didn't shut down cleanly), remove it and rebind.
/// If something *is* accepting, refuses to steal the path out from under a
/// live instance -- which is what keeps a racing second spawn from orphaning
/// the terminals the first one owns.
async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another horizon-terminald running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "horizon-terminald: removing stale socket {} (nothing was accepting)",
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

fn socket_path_from_args<I: Iterator<Item = String>>(mut args: I) -> Option<PathBuf> {
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
