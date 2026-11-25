//! `horizon-agent-acp`: Horizon's agent as an ACP agent over standard input
//! and output, one client per process (`docs/agent-stdio-child-design.md`).
//! The shell starts it as its child; so can any ACP client. It composes the
//! host (`horizon-agent-host`) with the board and the wiki, serves the one
//! connection its stdio is, and ends when the client closes it (or on
//! SIGTERM), flushing the session logs first. Its diagnostics go to
//! standard error. A test reaches its gates through the environment.
//!
//! The session logs are read and the sessions resumed on a background task
//! (`horizon_agent_host::startup::spawn_resume_task`); `initialize` is
//! answered meanwhile, and the methods that read sessions wait for it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use horizon_agent::registry::ProviderRegistry;
use horizon_agent_host::extension::Extensions;
use horizon_agent_host::hub::{self, flush_event_log_before_exit};
use horizon_agent_host::session::AgentdState;
use horizon_agent_host::startup::{StartupHooks, spawn_resume_task};
use horizon_agent_host::{outbox, providers};
use horizon_board_flow::BoardFlow;
use horizon_wiki_flow::WikiFlow;
use tokio::io::{AsyncRead, AsyncWrite};

/// Reported in this binary's `initialize` reply, as `_meta.horizon.binary_id`.
const BINARY_ID: &str = concat!("horizon-agent-acp/", env!("CARGO_PKG_VERSION"));

/// Test-only hook (`crates/horizon-agent-acp/tests/e2e.rs`): when set to a
/// file's path, the host's startup waits until that file is there before
/// opening the event log, so a test can prove that
/// `initialize` answers while the resume is held (and `session/list`/
/// `session/resume` don't) instead of relying on incidental timing. Never
/// set in production.
const TEST_RESUME_GATE_VAR: &str = "HORIZON_AGENT_TEST_RESUME_GATE";

/// Test-only hook (`crates/horizon-agent-acp/tests/e2e.rs`): when set to a
/// file's path, the mock provider's slow turns wait until that file is
/// there once their first word is out
/// (`ProviderRegistry::hold_mock_streams`), so a test can reattach while a
/// turn is surely streaming. Never set in production.
const TEST_MOCK_STREAM_GATE_VAR: &str = "HORIZON_AGENT_TEST_MOCK_STREAM_GATE";

/// Test-only hook (`horizon-daemon-testkit`'s hermetic command): when set,
/// sessions record no file checkpoints and a fork gets no worktree, so a
/// test's sessions leave the repository they run in alone.
const TEST_NO_CHECKPOINTS_VAR: &str = "HORIZON_AGENT_TEST_NO_CHECKPOINTS";

/// The file the variable `var` names, when it is set: a test's gate
/// ([`TEST_RESUME_GATE_VAR`]). The
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // One accepted snapshot supplies conversation and auxiliary providers.
    let raw_config = horizon_config::load();
    let fence = (raw_config.sandbox.fence_path())
        .unwrap_or_else(horizon_agent::tools::sandbox::fence_on_path);
    horizon_agent::tools::sandbox::configure(std::sync::Arc::new(
        horizon_sandbox::runtime::FenceRuntime::new(fence),
    ));
    if let Err(unavailable) = horizon_agent::tools::sandbox::availability() {
        eprintln!(
            "horizon-agent: the sandbox runtime is unavailable ({unavailable}); every bash \
             call will ask to run outside a sandbox"
        );
    }
    let extensions = extensions(std::env::current_dir().ok());
    let mut agent_config = providers::agent_config(raw_config);
    agent_config.rig.packages = extensions.packages();
    // Resolved once at startup; a provider reload reads it again.
    let config_path = horizon_config::resolved_path();
    eprintln!("horizon-agent: starting (model={})", agent_config.rig.model);

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
            config_path,
            horizon_config::project_grants(raw_config),
            horizon_config::trusted_projects(raw_config),
        )
        .with_checkpoints(checkpoints)
        .with_extensions(extensions.clone()),
    );
    // The shell that started this process puts its control plane here;
    // `workspace_snapshot` asks it.
    let state = match std::env::var_os("HORIZON_SOCKET").filter(|socket| !socket.is_empty()) {
        Some(socket) => Arc::new(
            Arc::into_inner(state)
                .expect("nothing holds the state yet")
                .with_control_socket(PathBuf::from(socket)),
        ),
        None => state,
    };

    spawn_resume_task(
        state.clone(),
        agent_config,
        StartupHooks {
            before_resume: test_gate(TEST_RESUME_GATE_VAR)
                .map(|gate| Box::new(move || wait_for_gate(&gate)) as Box<dyn FnOnce() + Send>),
            refused: Box::new(|error| {
                eprintln!("horizon-agent: startup refused: {error:#}");
                std::process::exit(1);
            }),
        },
    );
    outbox::spawn(
        state.clone(),
        extensions.trigger_sources(),
        extensions.reply_sinks(),
    );

    run(tokio::io::stdin(), tokio::io::stdout(), state).await;
    exit_now()
}

/// Serves the client on `read`/`write` until it closes the connection or
/// SIGTERM arrives, then flushes the session logs: an append only
/// enqueues, so ending without the flush would drop what the writer thread
/// has not taken yet.
async fn run<R, W>(read: R, write: W, state: Arc<AgentdState>)
where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(signal) => Some(signal),
        Err(error) => {
            eprintln!("horizon-agent: cannot watch for SIGTERM: {error}");
            None
        }
    };
    let terminated = async {
        match sigterm.as_mut() {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        served = hub::serve(read, write, state.clone(), BINARY_ID) => {
            if let Err(error) = served {
                eprintln!("horizon-agent: the connection failed: {error}");
            }
        }
        () = terminated => eprintln!("horizon-agent: terminated"),
    }
    flush_event_log_before_exit(state.writer());
}

/// Ends the process once the logs are flushed.
fn exit_now() -> ! {
    // `_exit`, not `exit`: `exit` runs the process's exit handlers and
    // static destructors while the sessions' threads are still running,
    // and the process then ends in a segmentation fault some of the time.
    // Nothing they would do is wanted: the logs are flushed, and standard
    // error is not buffered.
    // SAFETY: `_exit` only ends the process.
    unsafe { libc::_exit(0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::persistence::event_log::{Record, WriterHandle};
    use std::time::Duration;

    /// How long the writer thread holds its first write below. The
    /// test's own correctness does not depend on this number: with the
    /// flush in place, [`run`]'s SIGTERM arm blocks on a real writer
    /// barrier, so a slower machine only means a longer wait. It is what
    /// makes the *absence* of the flush observable -- without it, `run`
    /// returns while the writer thread is still inside this sleep and the
    /// log file is still empty when the assertion below reads it.
    const GATED_FIRST_WRITE: Duration = Duration::from_millis(750);

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
    /// The writer's first write is deliberately held (see
    /// [`GATED_FIRST_WRITE`]), which is what pins the three appends below
    /// in the channel -- unwritten -- at the moment SIGTERM arrives.
    #[tokio::test]
    async fn sigterm_flushes_the_event_log_before_exiting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");

        let writer = WriterHandle::open_after(&sessions, || std::thread::sleep(GATED_FIRST_WRITE));
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
            None,
            Vec::new(),
            Vec::new(),
        ));
        // The client keeps its end open: only SIGTERM ends the serve.
        let (_client, served) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(served);

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
        run(read, write, state).await;
        raiser.abort();

        let report =
            horizon_agent::persistence::event_log::read_all(&sessions).expect("read the logs");
        assert_eq!(
            report.records.len(),
            appended as usize,
            "SIGTERM must not exit before the event log's writer thread has flushed \
             everything already enqueued"
        );
    }
}
