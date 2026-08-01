//! `horizon-terminald`: the terminal daemon
//! (`docs/terminald-split-design.md`). Owns every PTY and the terminal
//! emulation loops (`horizon-terminal-core`), served over its own unix
//! socket as the [`TerminalHub`] rtc trait — a sibling of, and completely
//! independent from, `horizon-agentd`'s agent hub.
//!
//! **Why this is a separate process.** `Reload Agent Runtime` restarts the
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
//! lesson; decision 6, implemented client-side in `src/runtime/`).
//!
//! **No persistence, no readiness gate.** Unlike agentd, this daemon owns
//! no event log and no DuckDB projection, so there is nothing to resume at
//! startup and nothing to flush on drain: bind, accept, serve. A terminal
//! session's whole state lives in its PTY and its emulator, and the
//! backstop for losing this process is the workspace snapshot restore,
//! exactly as before.
//!
//! **Dependency-graph note.** No agent crate appears anywhere in this
//! binary's graph -- `cargo tree -p horizon-terminald -e normal` has
//! neither `horizon-agent` nor libduckdb. The socket-path convention it
//! used to reach into `horizon-agent` for lives in `horizon-wire`, the
//! domain-free foundation both daemons share
//! (`docs/runtime-crate-alignment-design.md` phase 1), and phase 2 moved
//! `TerminalHub` itself out of the union protocol crate and into
//! `horizon_terminal_core::wire`, dissolving the last transitive edge.
//! (`tests/e2e.rs` alone dev-depends on `horizon-agent`: the split's
//! acceptance property is that draining a *real* agentd leaves this
//! daemon's PTYs alive.)

mod hub;
mod terminal;

use std::sync::Arc;

use horizon_terminal_core::wire::TerminalHubServerShared;
use horizon_wire::daemon;
use horizon_wire::socket::default_terminald_socket_path;
use horizon_wire::WireCodec;
use hub::Hub;
use terminal::TerminalHost;
use tokio::net::UnixStream;

/// This daemon's name in every log line and diagnostic, including the ones
/// `horizon-wire`'s shared daemon plumbing emits on its behalf.
const DAEMON_NAME: &str = "horizon-terminald";

/// Reported in this binary's `hello` reply's `binary_id` — the value the
/// client records for decision 6's skew message. The negotiated protocol
/// version is carried separately in the same `TerminalHubHello`.
const BINARY_ID: &str = concat!("horizon-terminald/", env!("CARGO_PKG_VERSION"));

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = daemon::socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_terminald_socket_path);
    eprintln!("horizon-terminald: starting on {}", socket_path.display());

    let listener = daemon::bind_listener(&socket_path, DAEMON_NAME).await?;
    let terminals = TerminalHost::new();
    // The accept loop is `horizon-wire`'s ("bind, accept, serve, unlink" is
    // the same in both daemons); the SIGTERM hook is not, and this is the
    // destructive half of the terminald split: stopping this process means
    // every PTY it owns dies with it, whereas agentd's hook flushes an event
    // log and kills nothing. That asymmetry must stay an explicit hook.
    daemon::run(
        listener,
        &socket_path,
        DAEMON_NAME,
        |stream| handle_connection(stream, terminals.clone()),
        || terminals.shutdown_all(),
    )
    .await
}

/// Builds one connection's [`Hub`] and serves it for as long as the client
/// lives ([`daemon::serve_connection`] owns the remoc handshake, the size
/// caps, and the serve loop).
///
/// A dropped connection is *not* a session lifecycle event: the terminals
/// keep running (process-scoped), which is the whole point — a UI restart,
/// or a `Reload Agent Runtime` that happens to reconnect everything, finds
/// them alive and re-attaches. Only their subscriber bridges to the departed
/// client are cleared.
async fn handle_connection(stream: UnixStream, terminals: TerminalHost) -> anyhow::Result<()> {
    let hub = Hub::new(terminals.clone(), BINARY_ID);
    daemon::serve_connection::<_, TerminalHubServerShared<_, WireCodec>>(
        stream,
        DAEMON_NAME,
        Arc::new(hub),
        || terminals.clear_subscribers(),
    )
    .await
}
