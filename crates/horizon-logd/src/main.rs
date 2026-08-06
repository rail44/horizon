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
//! **Dependency-graph note.** This crate depends on `horizon-board` (for the
//! append logic and the `BoardEvent`/`Envelope` types it reuses), but the
//! wire types (`LogHub` trait, version pair) live in `horizon-board` too —
//! not here — to break the circular dependency that would arise if the board
//! library (the client) depended on this crate (the daemon) for the wire
//! types while this crate depended on the board library for the append logic.

use std::sync::Arc;

use horizon_board::wire::LogHubServerShared;
use horizon_logd::Hub;
use horizon_wire::daemon;
use horizon_wire::socket::default_logd_socket_path;
use horizon_wire::WireCodec;
use tokio::net::UnixStream;

/// This daemon's name in every log line and diagnostic.
const DAEMON_NAME: &str = "horizon-logd";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = daemon::socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_logd_socket_path);
    eprintln!("horizon-logd: starting on {}", socket_path.display());

    let listener = daemon::bind_listener(&socket_path, DAEMON_NAME).await?;
    // The accept loop is `horizon-wire`'s (same as both other daemons); the
    // SIGTERM hook is not, but logd has nothing to flush — it writes through
    // per-call — so the hook is a no-op.
    daemon::run(
        listener,
        &socket_path,
        DAEMON_NAME,
        handle_connection,
        || {},
    )
    .await
}

/// Builds one connection's [`Hub`] and serves it for as long as the client
/// lives ([`daemon::serve_connection`] owns the remoc handshake, the size
/// caps, and the serve loop).
async fn handle_connection(stream: UnixStream) -> anyhow::Result<()> {
    let hub = Hub::new();
    daemon::serve_connection::<_, LogHubServerShared<_, WireCodec>>(
        stream,
        DAEMON_NAME,
        Arc::new(hub),
        || {},
    )
    .await
}
