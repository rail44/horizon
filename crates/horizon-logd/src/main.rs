//! `horizon-logd`: the log daemon (`docs/logd-design.md`). Owns board writes
//! (the exclusive-flock append that used to live in `horizon-board`'s `Store`)
//! and serves them over its own unix socket as the `LogHub` rtc trait — a
//! sibling of, and completely independent from, `horizon-agentd` and
//! `horizon-terminald`.
//!
//! **No persistence, no readiness gate.** Like terminald, this daemon owns no
//! event log of its own: bind, accept, serve. The board events.jsonl is
//! written through (appended + flushed) before each `ingest` reply returns.
//!
//! **Stage B: subscribe.** The accept loop is logd-local (not the shared
//! `daemon::run`, which serves one connection at a time — a long-lived
//! streaming subscriber would block every ingest). Each connection is
//! spawned as its own task; the first byte is sniffed to route `{` → raw
//! NDJSON subscribe, anything else → remoc chmux `ingest`. One socket, two
//! transports, multiplexed by the first byte a shell script sends.
//!
//! **Dependency-graph note.** This crate depends on `horizon-board` (for the
//! append logic and the `BoardEvent`/`Envelope` types it reuses), but the
//! wire types (`LogHub` trait, version pair) live in `horizon-board` too —
//! not here — to break the circular dependency that would arise if the board
//! library (the client) depended on this crate (the daemon) for the wire
//! types while this crate depended on the board library for the append logic.

use std::sync::Arc;

use horizon_board::wire::LogHubServerShared;
use horizon_logd::{handle_subscribe, Hub, SubscriberRegistry};
use horizon_wire::daemon;
use horizon_wire::socket::default_logd_socket_path;
use horizon_wire::WireCodec;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;

/// This daemon's name in every log line and diagnostic.
const DAEMON_NAME: &str = "horizon-logd";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = daemon::socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_logd_socket_path);
    eprintln!("horizon-logd: starting on {}", socket_path.display());

    let listener = daemon::bind_listener(&socket_path, DAEMON_NAME).await?;

    // Process-wide subscriber registry, shared across all connections.
    let registry = Arc::new(SubscriberRegistry::new());

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                let registry = registry.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(stream, registry).await {
                        eprintln!("horizon-logd: connection error: {err}");
                    }
                });
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-logd: SIGTERM received, shutting down");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Routes one accepted connection to the remoc `ingest` path or the raw
/// NDJSON `subscribe` path by sniffing the first byte. A `{` (0x7B) byte
/// routes to subscribe (the subscriber's request line starts with a JSON
/// object); anything else routes to the remoc chmux handshake (whose first
/// byte is binary, never ASCII `{`).
///
/// The sniff uses `BufReader::fill_buf`, which peeks without consuming —
/// the byte stays in the reader's buffer for the handler that follows (both
/// the subscribe handler and the remoc handshake read from the same
/// `BufReader`, so the peeked byte is not lost). `tokio::net::UnixStream`
/// has no `peek` (unlike `TcpStream`), and `std`'s Unix-socket peek is
/// unstable, so buffering is the stable way to look ahead.
async fn handle_connection(
    stream: UnixStream,
    registry: Arc<SubscriberRegistry>,
) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);

    // Peek the first byte without consuming it.
    let buf = reader.fill_buf().await?;
    if buf.is_empty() {
        return Ok(()); // client closed before sending
    }

    if buf[0] == b'{' {
        // Raw NDJSON subscribe path.
        handle_subscribe(reader, write_half, registry).await
    } else {
        // remoc chmux ingest path — same as the other two daemons, but via
        // the generic-halves variant so the peeked byte rides along.
        let hub = Hub::new(registry);
        daemon::serve_connection_halves::<_, LogHubServerShared<_, WireCodec>, _, _>(
            reader,
            write_half,
            DAEMON_NAME,
            Arc::new(hub),
            || {},
        )
        .await
    }
}
