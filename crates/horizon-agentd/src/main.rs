//! `horizon-agentd`: steps 2-4 of `docs/agent-runtime-split-design.md`'s
//! agent-runtime split. Owns the Unix socket, the `hello` handshake, and (as
//! of step 3) real agent sessions: the hub's `new_agent` spawns the
//! provider/tool/persistence machinery this binary hosts (see
//! `session::run_session`), commands and events route by session id, and
//! this process owns the event log + DuckDB projection -- Horizon never
//! opens either itself. As of step 4, every session found in the log at
//! startup is resumed live (see `session::resume_persisted_sessions`) and
//! `attach_agent` re-emits a session's committed events to a (re)connecting
//! client.
//!
//! **Bind first (startup ordering).** [`main`] binds/listens on the socket
//! as its first action after arg/config parsing -- before it ever reads the
//! event log or resumes a single session. A large event log's read plus
//! resuming many sessions can take real wall-clock time (seconds, not
//! milliseconds), and the old ordering did all of that *before* binding: a
//! client racing to connect during that window would time out its own
//! retry budget, conclude nothing was listening, and spawn a second
//! `horizon-agentd` -- which itself would replay the whole log a second
//! time before discovering the first instance already owns the socket (see
//! `horizon_wire::daemon::bind_listener`'s stale-socket handling). Binding
//! first makes that whole failure mode structurally impossible in the
//! normal path: a client's
//! `connect` succeeds (queued by the kernel) the instant `listen` returns,
//! long before any persistence work starts, and a genuine second instance
//! now hits that function's "already accepting" bail immediately, before
//! it ever opens its own log.
//!
//! The event-log read and session resume move to a background task
//! ([`spawn_resume_task`]) that races the accept loop. `hello` -- the one
//! [`Hub`] method that never touches session state -- is answered
//! immediately regardless of whether that background work has finished;
//! `list_agents`/`attach_agent`/`new_agent` would return an incomplete (or,
//! right after bind, empty) view of history if answered too early, so they
//! block on [`session::AgentdState::wait_until_resume_ready`] first -- a
//! readiness gate, not a protocol change.
//!
//! **The DuckDB rebuild is off the readiness path too.** It used to run
//! synchronously inside [`open_persistence`], *before* [`AgentdState::
//! set_writer`]/[`session::resume_persisted_sessions`]/[`AgentdState::
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
//! `init_rx.recv()` -- and therefore [`AgentdState::mark_resume_ready`] --
//! actually waits on) and before it starts draining/durably writing further
//! appends. Readiness still doesn't wait on DuckDB at all, and the same
//! skip-when-current freshness check still applies (see that module's
//! `rebuild_and_open_duckdb_projection` doc comment) -- but the writer
//! thread now *keeps* the opened `Store` afterward instead of dropping it,
//! projecting every later append live instead of only at the next restart
//! (`docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum).

mod hub;
mod session;
mod worktree;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::ProviderRegistry;
use horizon_agent::persistence::event_log::{Record, WriterHandle, WriterInit};
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};
use horizon_agent::wire::SessionHubServerShared;
use horizon_wire::daemon;
use horizon_wire::socket::default_agentd_socket_path;
use horizon_wire::WireCodec;
use hub::{flush_event_log_before_exit, Hub};
use session::{AgentdState, Connection};
use tokio::net::{UnixListener, UnixStream};

/// This daemon's name in every log line and diagnostic, including the ones
/// `horizon-wire`'s shared daemon plumbing emits on its behalf.
const DAEMON_NAME: &str = "horizon-agentd";

/// Reported in this binary's `hello` reply's `binary_id`. The negotiated
/// protocol version is carried separately in the same `HubHello`.
const BINARY_ID: &str = concat!("horizon-agentd/", env!("CARGO_PKG_VERSION"));

/// Test-only hook (`crates/horizon-agentd/tests/e2e.rs`): when set to a
/// number of milliseconds, [`spawn_resume_task`] sleeps that long before
/// opening the event log, so a test can prove the bind-first ordering
/// (hello answers well before this delay elapses; `list_agents`/
/// `attach_agent` don't) instead of relying on incidental timing. Never set
/// in production.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_RESUME_DELAY_MS";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = daemon::socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_agentd_socket_path);

    // `horizon-agentd` is now the one process that reads Horizon's config
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
    // `ToolSessionState` (see `AgentdState::config_path`/`run_session`):
    // the `config.read`/`config.write` agent tools' one and only target.
    let config_path = horizon_config::resolved_path();
    eprintln!(
        "horizon-agentd: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    // Bind-first (see the module doc): this is the first thing that touches
    // the socket path, and it happens before the event log is even opened.
    let listener = daemon::bind_listener(&socket_path, DAEMON_NAME).await?;

    // Shared, multi-reader-blocking handle onto the live DuckDB projection
    // -- see `SharedDuckdbStore`'s doc comment. Created empty here (before
    // the event log's writer thread, and therefore any real DuckDB store,
    // exists) and handed to both the provider registry (the rig provider's
    // history replay) and `AgentdState` (the recall tools' context);
    // populated later, once, by `spawn_resume_task`'s propagator.
    let duckdb_cell = SharedDuckdbStore::new();
    let providers =
        ProviderRegistry::builtin_with_config(agent_config.clone(), duckdb_cell.clone());
    let state = Arc::new(AgentdState::new(
        providers,
        agent_config.clone(),
        None,
        duckdb_cell.clone(),
        config_path,
        horizon_config::project_grants(raw_config),
    ));

    spawn_resume_task(state.clone(), agent_config, duckdb_cell);

    run(listener, &socket_path, state).await
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
    state: Arc<AgentdState>,
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
        // Step 4: "agentd restart = read own log, rebuild rig_history, mark
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
/// [`spawn_resume_task`] can stash it on [`AgentdState`] for the [`Hub`]'s
/// `hello` to forward to a connecting client over `HubHello::skipped_lines`
/// -- restoring the step-3 trim recorded in
/// `docs/agent-runtime-split-design.md`
/// ("Skipped-lines status reporting is omitted").
///
/// Opens via [`WriterHandle::open_silently`] rather than [`WriterHandle::
/// open`]: this function already prints its own `horizon-agentd`-prefixed
/// skipped-lines summary just below, so the shared writer module's own
/// generic summary line would otherwise double up in agentd's stderr.
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
                    "horizon-agentd: {summary} while opening {}",
                    agent_config.persistence.event_log_path.display()
                );
            }
            (Some(writer), report.records, skipped_summary, duckdb_rx)
        }
        Ok(WriterInit::Failed(error)) => {
            eprintln!(
                "horizon-agentd: event log unavailable ({error}); persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
        Err(_) => {
            eprintln!(
                "horizon-agentd: event log writer thread exited before reporting startup status; \
                 persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
    }
}

/// The accept loop, over `horizon-wire`'s shared daemon lifecycle
/// ([`daemon::run`]): everything about "bind, accept, serve, unlink" is the
/// same in both daemons, and the one thing that is not — what SIGTERM has
/// to do before this process may exit — is the hook passed below. Here that
/// is the same durability obligation a `drain` has, for the same reason: an
/// `append` only enqueues, so exiting without flushing drops whatever the
/// writer thread hadn't dequeued yet (see [`flush_event_log_before_exit`]).
/// `horizon-terminald`'s hook kills its PTYs instead, and that asymmetry is
/// the terminald split itself.
async fn run(
    listener: UnixListener,
    socket_path: &Path,
    state: Arc<AgentdState>,
) -> anyhow::Result<()> {
    let shutdown_state = state.clone();
    daemon::run(
        listener,
        socket_path,
        DAEMON_NAME,
        |stream| handle_connection(stream, state.clone()),
        move || flush_event_log_before_exit(shutdown_state.writer()),
    )
    .await
}

/// Builds one connection's [`Hub`] and serves it for as long as the client
/// lives ([`daemon::serve_connection`] owns the remoc handshake, the size
/// caps, and the serve loop). The [`Connection`] is this daemon's
/// per-connection seam onto process-lifetime state, so it is also what the
/// post-connection cleanup closes: the sessions themselves keep running
/// (they are scoped to the process, not the connection), but their bridges
/// to this connection are dead.
async fn handle_connection(stream: UnixStream, state: Arc<AgentdState>) -> anyhow::Result<()> {
    let connection = Connection::new(state);
    let hub = Hub::new(connection.clone(), BINARY_ID);
    daemon::serve_connection::<_, SessionHubServerShared<_, WireCodec>>(
        stream,
        DAEMON_NAME,
        Arc::new(hub),
        || connection.disconnect(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long the writer thread's startup read is held open below. The
    /// test's own correctness does not depend on this number: with the
    /// flush in place, [`run`]'s SIGTERM arm blocks on a real writer
    /// barrier, so a slower machine only means a longer wait. It is what
    /// makes the *absence* of the flush observable -- without it, `run`
    /// returns while the writer thread is still inside this sleep and the
    /// log file is still empty when the assertion below reads it.
    const GATED_STARTUP_READ: Duration = Duration::from_millis(750);

    fn state_record(session_id: horizon_agent::contract::SessionId, sequence: u64) -> Record {
        Record {
            schema: "horizon.agent.event_log".to_string(),
            version: 1,
            event_id: uuid::Uuid::new_v4().to_string(),
            sequence,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: None,
            session_context: None,
            event_kind: "state_changed".to_string(),
            event: horizon_agent::contract::Event::StateChanged(
                horizon_agent::contract::SessionState::Running,
            ),
            provider_payload: None,
            created_at_unix_ms: sequence + 1,
        }
    }

    /// The durability obligation of the SIGTERM path: a plain `kill` --
    /// what an operator is told to use to stop this daemon by hand -- must
    /// flush the event log, exactly as a `drain` does. `append` only
    /// enqueues onto the writer thread's channel, so before the flush
    /// landed, records that had already been forwarded to a client over the
    /// wire could still die with the process.
    ///
    /// The writer's startup read is deliberately held open (see
    /// [`GATED_STARTUP_READ`]), which is what pins the three appends below
    /// in the channel -- unwritten -- at the moment SIGTERM arrives.
    #[tokio::test]
    async fn sigterm_flushes_the_event_log_before_exiting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("events.jsonl");
        let socket_path = dir.path().join("agentd.sock");

        let (writer, init_rx) = WriterHandle::open_with_reader(&log_path, |path| {
            std::thread::sleep(GATED_STARTUP_READ);
            horizon_agent::persistence::event_log::read(path)
        });
        let session_id = horizon_agent::contract::SessionId::new();
        let appended = 3;
        for sequence in 0..appended {
            writer
                .append(state_record(session_id, sequence))
                .expect("append enqueues while the startup read is still gated");
        }

        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            Some(writer),
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
        ));
        let listener = daemon::bind_listener(&socket_path, DAEMON_NAME)
            .await
            .expect("bind");

        // Installed before anything raises SIGTERM, so this process can
        // never take the default (fatal) disposition: tokio's handler is
        // process-global and outlives every `Signal` stream built on it,
        // including the one `run` makes for itself.
        let _sigterm_guard =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install the process-wide SIGTERM handler");

        // Spawned with no `.await` between here and `run` below, so on the
        // current-thread runtime `run` has already built its own signal
        // stream by the time this task first runs. The repeat is belt and
        // braces against a stream that subscribed a moment too late: a
        // watch receiver never sees a value sent before it existed.
        let raiser = tokio::spawn(async {
            loop {
                // SAFETY: `raise` is async-signal-safe and targets this
                // process, whose SIGTERM disposition tokio already owns.
                unsafe { libc::raise(libc::SIGTERM) };
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        run(listener, &socket_path, state).await.expect("run");
        raiser.abort();

        match init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(_) => {}
            WriterInit::Failed(error) => panic!("unexpected startup failure: {error}"),
        }
        let report = horizon_agent::persistence::event_log::read(&log_path).expect("read log");
        assert_eq!(
            report.records.len(),
            appended as usize,
            "SIGTERM must not exit before the event log's writer thread has flushed \
             everything already enqueued"
        );
    }
}
