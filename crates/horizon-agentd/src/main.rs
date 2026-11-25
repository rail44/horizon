//! `horizon-agentd`: the agent host (`horizon-agent-host`) behind a Unix
//! socket, composed with the board and the wiki (`docs/agent-runtime-split-design.md`;
//! `docs/agent-stdio-child-design.md` replaces it). It owns the socket, the
//! accept loop, what a drain and SIGTERM do to the process, and the test
//! gates a spawning test reaches through the environment; the sessions, the
//! event log and the ACP agent side are the host's.
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
//! (`horizon_agent_host::startup::spawn_resume_task`) that races the accept loop. `initialize` -- the one
//! method that never touches session state -- is answered
//! immediately regardless of whether that background work has finished;
//! `session/list`/`session/resume`/`session/new` would return an incomplete (or,
//! right after bind, empty) view of history if answered too early, so they
//! block on the host's resume readiness first -- a
//! readiness gate, not a protocol change.
//!

use std::path::{Path, PathBuf};
use std::sync::Arc;

use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
use horizon_agent::registry::ProviderRegistry;
use horizon_agent_host::DAEMON_NAME;
use horizon_agent_host::extension::Extensions;
use horizon_agent_host::hub::{self, flush_event_log_before_exit};
use horizon_agent_host::session::AgentdState;
use horizon_agent_host::startup::{StartupHooks, spawn_resume_task};
use horizon_agent_host::{outbox, providers};
use horizon_board_flow::BoardFlow;
use horizon_wiki_flow::WikiFlow;
use horizon_wire::daemon;
use horizon_wire::socket::default_agentd_socket_path;
use tokio::net::{UnixListener, UnixStream};
/// Reported in this binary's `initialize` reply, as `_meta.horizon.binary_id`.
const BINARY_ID: &str = concat!("horizon-agentd/", env!("CARGO_PKG_VERSION"));

/// Test-only hook (`crates/horizon-agentd/tests/e2e.rs`): when set to a
/// file's path, the host's startup waits until that file is there before
/// opening the event log, so a test can prove the bind-first ordering
/// (`initialize` answers while the resume is held; `session/list`/
/// `session/resume` don't) instead of relying on incidental timing. Never
/// set in production.
const TEST_RESUME_GATE_VAR: &str = "HORIZON_AGENTD_TEST_RESUME_GATE";

/// Test-only hook (`crates/horizon-agentd/tests/e2e.rs`): when set to a
/// file's path, the full DuckDB rebuild waits until that file is there
/// (the writer's `before_rebuild`), so a test can
/// prove the rebuild never sits on the readiness path `session/list` and
/// `session/new` block on. Never set in production.
const TEST_DUCKDB_REBUILD_GATE_VAR: &str = "HORIZON_AGENTD_TEST_DUCKDB_REBUILD_GATE";

/// Test-only hook (`crates/horizon-agentd/tests/e2e.rs`): when set to a
/// file's path, the mock provider's slow turns wait until that file is
/// there once their first word is out
/// (`ProviderRegistry::hold_mock_streams`), so a test can reattach while a
/// turn is surely streaming. Never set in production.
const TEST_MOCK_STREAM_GATE_VAR: &str = "HORIZON_AGENTD_TEST_MOCK_STREAM_GATE";

/// Test-only hook (`horizon-daemon-testkit`'s hermetic command): when set,
/// sessions record no file checkpoints and a fork gets no worktree, so a
/// test's sessions leave the repository they run in alone.
const TEST_NO_CHECKPOINTS_VAR: &str = "HORIZON_AGENTD_TEST_NO_CHECKPOINTS";

/// The file the variable `var` names, when it is set: a test's gate
/// ([`TEST_RESUME_GATE_VAR`], [`TEST_DUCKDB_REBUILD_GATE_VAR`]). The
/// variable is how a test that spawns this daemon reaches it; inside, the
/// gate is an argument. A build without test fixtures never reads it.
fn test_gate(var: &str) -> Option<PathBuf> {
    if !horizon_agent::TEST_FIXTURES {
        return None;
    }
    std::env::var_os(var)
        .filter(|gate| !gate.is_empty())
        .map(PathBuf::from)
}

/// Waits until `gate` is there.
fn wait_for_gate(gate: &Path) {
    while !gate.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The board and the wiki, rooted at `start_dir`. What they add to the
/// agent (roles, skills and tools) is carried on the agent configuration
/// (`RigAgentConfig::packages`) to the providers and every session.
fn extensions(start_dir: Option<PathBuf>) -> Extensions {
    Extensions::new(vec![
        Arc::new(BoardFlow::new(start_dir.clone())),
        Arc::new(WikiFlow::new(start_dir)),
    ])
}

/// What a drain does once the log is flushed.
fn exit_now() {
    // `_exit`, not `exit`: `exit` runs the process's exit handlers and
    // static destructors while the sessions' threads are still running,
    // and the daemon then ends in a segmentation fault some of the time.
    // Nothing they would do is wanted: the log is flushed, and standard
    // error is not buffered.
    // SAFETY: `_exit` only ends the process.
    unsafe { libc::_exit(0) }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path = daemon::socket_path_from_args(std::env::args().skip(1))
        .unwrap_or_else(default_agentd_socket_path);

    // One accepted snapshot supplies conversation and auxiliary providers.
    let raw_config = horizon_config::load();
    let fence = (raw_config.sandbox.fence_path())
        .unwrap_or_else(horizon_agent::tools::sandbox::fence_on_path);
    horizon_agent::tools::sandbox::configure(std::sync::Arc::new(
        horizon_sandbox::runtime::FenceRuntime::new(fence),
    ));
    if let Err(unavailable) = horizon_agent::tools::sandbox::availability() {
        eprintln!(
            "horizon-agentd: the sandbox runtime is unavailable ({unavailable}); every bash \
             call will ask to run outside a sandbox"
        );
    }
    let extensions = extensions(std::env::current_dir().ok());
    let mut agent_config = providers::agent_config(raw_config);
    agent_config.rig.packages = extensions.packages();
    // Resolved once at startup; a provider reload reads it again.
    let config_path = horizon_config::resolved_path();
    eprintln!(
        "horizon-agentd: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    // Bind-first (see the module doc): this is the first thing that touches
    // the socket path, and it happens before the event log is even opened.
    let listener = daemon::bind_listener(&socket_path, DAEMON_NAME).await?;

    // The shared slot for the live DuckDB projection -- see
    // `SharedDuckdbStore`'s doc comment. Created pending here (before
    // the event log's writer thread, and therefore any real DuckDB store,
    // exists) and handed to `AgentdState` (the recall tools' context);
    // populated later, once, by `spawn_resume_task`'s propagator.
    let duckdb_cell = SharedDuckdbStore::pending();
    let mut providers = ProviderRegistry::builtin_with_config(agent_config.clone());
    if let Some(gate) = test_gate(TEST_MOCK_STREAM_GATE_VAR) {
        providers.hold_mock_streams(Arc::new(move || wait_for_gate(&gate)));
    }
    let checkpoints = !(horizon_agent::TEST_FIXTURES
        && std::env::var_os(TEST_NO_CHECKPOINTS_VAR).is_some_and(|set| !set.is_empty()));
    let state = Arc::new(
        AgentdState::new(
            providers,
            agent_config.clone(),
            None,
            duckdb_cell.clone(),
            config_path,
            horizon_config::project_grants(raw_config),
            horizon_config::trusted_projects(raw_config),
        )
        .with_checkpoints(checkpoints)
        .with_extensions(extensions.clone()),
    );

    spawn_resume_task(
        state.clone(),
        agent_config,
        duckdb_cell,
        StartupHooks {
            before_resume: test_gate(TEST_RESUME_GATE_VAR)
                .map(|gate| Box::new(move || wait_for_gate(&gate)) as Box<dyn FnOnce() + Send>),
            before_rebuild: test_gate(TEST_DUCKDB_REBUILD_GATE_VAR)
                .map(|gate| Arc::new(move || wait_for_gate(&gate)) as Arc<dyn Fn() + Send + Sync>),
            refused: Box::new(|error| {
                eprintln!("horizon-agentd: startup refused: {error:#}");
                std::process::exit(1);
            }),
        },
    );
    outbox::spawn(
        state.clone(),
        extensions.trigger_sources(),
        extensions.reply_sinks(),
    );

    run(listener, &socket_path, state).await
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

/// Serves one connection's ACP agent side for as long as the client lives
/// ([`hub::serve`]). The sessions themselves keep running after it ends
/// (they are scoped to the process, not the connection); only their
/// bridges to this connection die.
async fn handle_connection(stream: UnixStream, state: Arc<AgentdState>) -> anyhow::Result<()> {
    let (read, write) = stream.into_split();
    hub::serve(read, write, state, BINARY_ID, Arc::new(exit_now))
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::persistence::event_log::{Record, WriterHandle, WriterInit};
    use std::time::Duration;

    /// How long the writer thread's startup read is held open below. The
    /// test's own correctness does not depend on this number: with the
    /// flush in place, [`run`]'s SIGTERM arm blocks on a real writer
    /// barrier, so a slower machine only means a longer wait. It is what
    /// makes the *absence* of the flush observable -- without it, `run`
    /// returns while the writer thread is still inside this sleep and the
    /// log file is still empty when the assertion below reads it.
    const GATED_STARTUP_READ: Duration = Duration::from_millis(750);

    /// Every skill a package adds parses: composing the registry panics on
    /// one that does not, which would stop every session at startup.
    #[test]
    fn every_skill_the_packages_add_parses() {
        let _ = horizon_agent::skills::SkillRegistry::embedded(&extensions(None).packages());
    }

    fn state_record(session_id: horizon_agent::contract::SessionId, sequence: u64) -> Record {
        Record {
            schema: "horizon.agent.event_log".to_string(),
            version: 5,
            event_id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
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

        let agent_config = horizon_agent_host::session::test_support::test_config();
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(agent_config.clone()),
            agent_config,
            Some(writer),
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
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
