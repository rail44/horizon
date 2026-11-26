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

use horizon_agent_acp::{Agent, Composition, Places, serve_until};
use horizon_agent_host::session::AgentdState;
use tokio::io::{AsyncRead, AsyncWrite};

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = horizon_config::load();
    let fence =
        (config.sandbox.fence_path()).unwrap_or_else(horizon_agent::tools::sandbox::fence_on_path);
    horizon_agent::tools::sandbox::configure(Arc::new(
        horizon_sandbox::runtime::FenceRuntime::new(fence),
    ));
    if let Err(unavailable) = horizon_agent::tools::sandbox::availability() {
        eprintln!(
            "horizon-agent: the sandbox runtime is unavailable ({unavailable}); every bash \
             call will ask to run outside a sandbox"
        );
    }
    let checkpoints = !(horizon_agent::TEST_FIXTURES
        && std::env::var_os(TEST_NO_CHECKPOINTS_VAR).is_some_and(|set| !set.is_empty()));
    let agent = Agent::compose(Composition {
        config: config.clone(),
        // Resolved once at startup; a provider reload reads it again.
        config_path: horizon_config::resolved_path(),
        places: Places::from_env(),
        start_dir: std::env::current_dir().ok(),
        // The shell that started this process puts its control plane here;
        // `workspace_snapshot` asks it.
        control_socket: std::env::var_os("HORIZON_SOCKET")
            .filter(|socket| !socket.is_empty())
            .map(PathBuf::from),
        checkpoints,
        before_resume: test_gate(TEST_RESUME_GATE_VAR)
            .map(|gate| Box::new(move || wait_for_gate(&gate)) as Box<dyn FnOnce() + Send>),
        hold_mock_streams: test_gate(TEST_MOCK_STREAM_GATE_VAR)
            .map(|gate| Arc::new(move || wait_for_gate(&gate)) as Arc<dyn Fn() + Send + Sync>),
        refused: Box::new(|error| {
            eprintln!("horizon-agent: startup refused: {error:#}");
            std::process::exit(1);
        }),
    });
    run(
        tokio::io::stdin(),
        tokio::io::stdout(),
        agent.state().clone(),
    )
    .await;
    exit_now()
}

/// Serves the client on `read`/`write` until it closes the connection or
/// SIGTERM arrives, then flushes the session logs.
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
    let terminated = async move {
        match sigterm.as_mut() {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    };
    serve_until(read, write, state, terminated).await;
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
            horizon_agent::registry::ProviderRegistry::builtin_with_config(agent_config.clone()),
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
