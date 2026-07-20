//! `horizon-sessiond`: steps 2-4 of `docs/agent-runtime-split-design.md`'s
//! agent-runtime split. Owns the Unix socket, the `hello` handshake, and (as
//! of step 3) real agent sessions: `session_new` spawns the provider/tool/
//! persistence machinery this binary hosts (see `session::run_session`),
//! command/event envelopes route by session id, and this process owns the
//! event log + DuckDB projection -- Horizon never opens either itself. As of
//! step 4, every session found in the log at startup is resumed live (see
//! `session::resume_persisted_sessions`) and `session_load` re-emits a
//! session's committed events to a (re)connecting client.
//!
//! **Bind first (startup ordering).** [`main`] binds/listens on the socket
//! as its first action after arg/config parsing -- before it ever reads the
//! event log or resumes a single session. A large event log's read plus
//! resuming many sessions can take real wall-clock time (seconds, not
//! milliseconds), and the old ordering did all of that *before* binding: a
//! client racing to connect during that window would time out its own
//! retry budget, conclude nothing was listening, and spawn a second
//! `horizon-sessiond` -- which itself would replay the whole log a second
//! time before discovering the first instance already owns the socket (see
//! `bind_listener`'s stale-socket handling). Binding first makes that whole
//! failure mode structurally impossible in the normal path: a client's
//! `connect` succeeds (queued by the kernel) the instant `listen` returns,
//! long before any persistence work starts, and a genuine second instance
//! now hits `bind_listener`'s "already accepting" bail immediately, before
//! it ever opens its own log.
//!
//! The event-log read and session resume move to a background task
//! ([`spawn_resume_task`]) that races the accept loop. `hello`/`ping` never
//! touch session state, so they're answered immediately regardless of
//! whether that background work has finished; `session_list`/`session_load`/
//! `session_new` would return an incomplete (or, right after bind, empty)
//! view of history if answered too early, so they block on [`session::
//! SessiondState::wait_until_resume_ready`] first -- a readiness gate, not a
//! protocol change.
//!
//! **The DuckDB rebuild is off the readiness path too.** It used to run
//! synchronously inside [`open_persistence`], *before* [`SessiondState::
//! set_writer`]/[`session::resume_persisted_sessions`]/[`SessiondState::
//! mark_resume_ready`] -- meaning every readiness-gated request waited on a
//! full synchronous rebuild of a derived, non-authoritative read model that
//! no session actually needs (sessions resume from the JSONL log directly).
//!
//! As of the recall work, the rebuild (and the projection itself) moved
//! again: it's no longer a separate task this binary spawns at all.
//! [`open_persistence`] now passes `agent_config.persistence.duckdb_path`
//! straight into [`WriterHandle::open_silently`], and the event log's own
//! background writer thread (`horizon_agent::persistence::event_log::
//! writer`) does the rebuild-or-skip itself, right after it sends
//! `WriterInit::Ready` (which is what [`open_persistence`]'s blocking
//! `init_rx.recv()` -- and therefore [`SessiondState::mark_resume_ready`] --
//! actually waits on) and before it starts draining/durably writing further
//! appends. Readiness still doesn't wait on DuckDB at all, and the same
//! skip-when-current freshness check still applies (see that module's
//! `rebuild_and_open_duckdb_projection` doc comment) -- but the writer
//! thread now *keeps* the opened `Store` afterward instead of dropping it,
//! projecting every later append live instead of only at the next restart
//! (`docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum).

mod hub;
mod session;
mod terminal;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::ProviderRegistry;
use horizon_agent::persistence::event_log::{Record, WriterHandle, WriterInit};
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};
use horizon_agent::socket::default_socket_path;
use horizon_session_protocol::{SessionHubClient, SessionHubServerShared, WireCodec};
use hub::Hub;
use remoc::rtc::ServerShared as _;
use session::{Connection, SessiondState};
use terminal::TerminalHost;
use tokio::net::{UnixListener, UnixStream};

/// Reported in this binary's `hello` reply's `binary_id`. The negotiated
/// protocol version is carried separately in the same `HubHello`.
const BINARY_ID: &str = concat!("horizon-sessiond/", env!("CARGO_PKG_VERSION"));

/// How long an accepted connection gets to complete the remoc (chmux)
/// handshake before the daemon gives up on it. A v10 client completes it
/// in milliseconds; what this bounds is a *JSONL-generation* (v<=9) peer —
/// whose line-framed hello is chmux garbage — or a port scanner, neither
/// of which may wedge the one-at-a-time accept loop for chmux's raw 60 s.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Test-only hook (`crates/horizon-sessiond/tests/e2e.rs`): when set to a
/// number of milliseconds, [`spawn_resume_task`] sleeps that long before
/// opening the event log, so a test can prove the bind-first ordering
/// (hello answers well before this delay elapses; `session_list`/
/// `session_load` don't) instead of relying on incidental timing. Never set
/// in production.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_SESSIOND_TEST_RESUME_DELAY_MS";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path =
        socket_path_from_args(std::env::args().skip(1)).unwrap_or_else(default_socket_path);

    // `horizon-sessiond` is now the one process that reads Horizon's config
    // file directly (see `docs/agent-runtime-split-design.md`'s "the child
    // owns the event log and DuckDB projection", extended by the
    // 2026-07-18 config-narrowing wave's consolidation onto
    // `horizon-config`): only `[provider]` `model`/`base_url` still vary
    // per this crate's config -- everything else former `[agent]` knobs
    // became fixed built-in constants in `horizon_agent::config` (see that
    // module's doc).
    let raw_config = horizon_config::load();
    let agent_config = AgentConfig::from_env_and_provider(
        raw_config.provider.model.clone(),
        raw_config.provider.base_url.clone(),
    );
    // Resolved once at startup and handed to every session's
    // `ToolSessionState` (see `SessiondState::config_path`/`run_session`):
    // the `config.read`/`config.write` agent tools' one and only target.
    let config_path = horizon_config::resolved_path();
    eprintln!(
        "horizon-sessiond: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    // Bind-first (see the module doc): this is the first thing that touches
    // the socket path, and it happens before the event log is even opened.
    let listener = bind_listener(&socket_path).await?;

    // Shared, multi-reader-blocking handle onto the live DuckDB projection
    // -- see `SharedDuckdbStore`'s doc comment. Created empty here (before
    // the event log's writer thread, and therefore any real DuckDB store,
    // exists) and handed to both the provider registry (the rig provider's
    // history replay) and `SessiondState` (the recall tools' context);
    // populated later, once, by `spawn_resume_task`'s propagator.
    let duckdb_cell = SharedDuckdbStore::new();
    let providers =
        ProviderRegistry::builtin_with_config(agent_config.clone(), duckdb_cell.clone());
    let state = Arc::new(SessiondState::new(
        providers,
        agent_config.clone(),
        None,
        duckdb_cell.clone(),
        config_path,
    ));

    spawn_resume_task(state.clone(), agent_config, duckdb_cell);
    let terminals = TerminalHost::new();

    run(listener, &socket_path, state, terminals).await
}

/// Opens this process's own event log writer, resumes every session found
/// in it, and marks `state` ready -- all on a background task that races
/// the accept loop `main` starts right after this returns, per the
/// module's bind-first fix. Decision 3 in the design doc ("the child owns
/// the event log and DuckDB projection") still holds: this uses this
/// binary's own config load, never Horizon's.
///
/// The DuckDB rebuild is no longer spawned from here at all -- see the
/// module doc's "DuckDB rebuild is off the readiness path too" for where it
/// actually happens now ([`open_persistence`]'s `WriterHandle::
/// open_silently` call).
///
/// `duckdb_cell` is populated by a *separate*, fire-and-forget
/// `spawn_blocking` task spawned right after `open_persistence` returns --
/// deliberately not awaited by (or ordered against) `resume_persisted_
/// sessions`/`mark_resume_ready` below, so populating it can never
/// reintroduce DuckDB onto the readiness path this function's own doc
/// comment describes avoiding.
fn spawn_resume_task(
    state: Arc<SessiondState>,
    agent_config: AgentConfig,
    duckdb_cell: SharedDuckdbStore,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(delay) = test_resume_delay() {
            std::thread::sleep(delay);
        }
        let (writer, records, skipped_lines_summary, duckdb_ready_rx) =
            open_persistence(&agent_config);
        state.set_writer(writer);
        state.set_skipped_lines_summary(skipped_lines_summary);
        // Step 4: "sessiond restart = read own log, rebuild rig_history, mark
        // turns that died mid-flight as cancelled ... sessions are live
        // again".
        session::resume_persisted_sessions(&state, records);
        state.mark_resume_ready();

        // Propagates the writer thread's own rebuild-or-open decision into
        // the shared cell whenever it lands -- `duckdb_ready_rx.recv()`
        // blocks this dedicated propagator task, never the resume/readiness
        // path above (already completed by the time this line runs). A
        // disconnected channel (the writer's startup itself failed, so it
        // never reached the DuckDB step at all) is treated the same as an
        // explicit "no store".
        tokio::task::spawn_blocking(move || {
            let store = duckdb_ready_rx.recv().ok().flatten();
            duckdb_cell.set(store);
        });
    });
}

fn test_resume_delay() -> Option<Duration> {
    std::env::var(TEST_RESUME_DELAY_MS_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Opens this process's own event log writer, using this binary's own
/// config load (never Horizon's). Blocks the calling (background, per
/// [`spawn_resume_task`]) task on the writer's startup read -- simpler than
/// threading the `WriterInit` channel further, and no longer something the
/// accept loop waits on (see the module doc). Also hands back the startup
/// read's records (empty if persistence is disabled or there's nothing yet)
/// so the caller can resume every session they belong to
/// ([`session::resume_persisted_sessions`]), and the human-readable
/// skipped-lines summary (if any corrupt/torn lines were found) so
/// [`spawn_resume_task`] can stash it on [`SessiondState`] for `main::
/// run_session_hosting_loop` to forward to a connecting client -- restoring
/// the step-3 trim recorded in `docs/agent-runtime-split-design.md`
/// ("Skipped-lines status reporting is omitted").
///
/// Opens via [`WriterHandle::open_silently`] rather than [`WriterHandle::
/// open`]: this function already prints its own `horizon-sessiond`-prefixed
/// skipped-lines summary just below, so the shared writer module's own
/// generic summary line would otherwise double up in sessiond's stderr.
/// `open_silently`'s `duckdb_path` argument is this binary's only real
/// production use of the live DuckDB projection (see the module doc): the
/// event log's own writer thread rebuilds (or skips, if already current)
/// and then keeps that `Store` open for the rest of this process's life.
///
/// The fourth return value is `open_silently`'s own second receiver: the
/// writer thread's DuckDB rebuild-or-open decision, delivered exactly once,
/// whenever it lands (see [`spawn_resume_task`]'s propagator, which is the
/// only consumer). A disconnected channel (writer startup itself failed)
/// is indistinguishable from -- and handled the same as -- an explicit
/// `None`.
fn open_persistence(
    agent_config: &AgentConfig,
) -> (
    Option<WriterHandle>,
    Vec<Record>,
    Option<String>,
    crossbeam_channel::Receiver<Option<DuckdbStoreHandle>>,
) {
    let (writer, init_rx, duckdb_rx) = WriterHandle::open_silently(
        &agent_config.persistence.event_log_path,
        agent_config.persistence.duckdb_path.clone(),
    );
    match init_rx.recv() {
        Ok(WriterInit::Ready(report)) => {
            let skipped_summary = report.skipped_summary();
            if let Some(summary) = &skipped_summary {
                eprintln!(
                    "horizon-sessiond: {summary} while opening {}",
                    agent_config.persistence.event_log_path.display()
                );
            }
            (Some(writer), report.records, skipped_summary, duckdb_rx)
        }
        Ok(WriterInit::Failed(error)) => {
            eprintln!(
                "horizon-sessiond: event log unavailable ({error}); persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
        Err(_) => {
            eprintln!(
                "horizon-sessiond: event log writer thread exited before reporting startup status; \
                 persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
    }
}

async fn run(
    listener: UnixListener,
    socket_path: &Path,
    state: Arc<SessiondState>,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = handle_connection(stream, state.clone(), terminals.clone()).await {
                    eprintln!("horizon-sessiond: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-sessiond: SIGTERM received, shutting down");
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
/// support is explicitly out of scope -- see the design doc's "Out of scope
/// here"). The v10 shape: establish the remoc connection (bounded by
/// [`CONNECT_TIMEOUT`] so a JSONL-generation peer's garbage can't wedge the
/// accept loop), hand the client its `SessionHubClient` over the base
/// channel, then serve the [`Hub`] with per-call task spawning until the
/// client goes away. Version negotiation is no longer a transport-level
/// concern at all -- it is the `hello` rtc call, answered by the hub
/// (`docs/remoc-adoption-design.md` §3); a range-rejected client can still
/// call `drain`, which is how the auto-recovery path restarts a stale
/// daemon at the right version.
async fn handle_connection(
    stream: UnixStream,
    state: Arc<SessiondState>,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let connect = remoc::Connect::io::<_, _, SessionHubClient<WireCodec>, (), WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, mut base_tx, _base_rx) = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await
    {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => {
            eprintln!(
                "horizon-sessiond: dropping a connection that failed the remoc handshake \
                 (a pre-v10 JSONL client, or not a Horizon client at all): {error}"
            );
            return Ok(());
        }
        Err(_elapsed) => {
            eprintln!(
                "horizon-sessiond: dropping a connection with no remoc handshake within \
                 {CONNECT_TIMEOUT:?} (a pre-v10 JSONL client, or not a Horizon client at all)"
            );
            return Ok(());
        }
    };
    // The chmux multiplexer must be polled for the connection to make any
    // progress (adoption condition 3) -- spawned, so it runs alongside the
    // serve loop below.
    let mut conn_task = tokio::spawn(conn);

    let connection = Connection::new(state);
    let hub = Hub::new(connection.clone(), terminals.clone(), BINARY_ID);
    let (server, client) = SessionHubServerShared::<_, WireCodec>::new(Arc::new(hub), 16);
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
                eprintln!("horizon-sessiond: hub serve error: {error}");
            }
        }
        _ = &mut conn_task => {}
    }

    // Post-connection cleanup: sessions keep running (they are scoped to
    // the process, not the connection), but their bridges to this
    // connection are dead.
    connection.disconnect();
    terminals.clear_subscribers();
    conn_task.abort();
    Ok(())
}

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// `horizon-sessiond` that didn't shut down cleanly), remove it and rebind.
/// If something *is* accepting, refuses to steal the path out from under a
/// live instance.
async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another horizon-sessiond running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "horizon-sessiond: removing stale socket {} (nothing was accepting)",
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
