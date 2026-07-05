//! `horizon-agentd`: steps 2-4 of `docs/agent-runtime-split-design.md`'s
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
//! `horizon-agentd` -- which itself would replay the whole log a second
//! time before discovering the first instance already owns the socket (see
//! `bind_listener`'s stale-socket handling). Binding first makes that whole
//! failure mode structurally impossible in the normal path: a client's
//! `connect` succeeds (queued by the kernel) the instant `listen` returns,
//! long before any persistence work starts, and a genuine second instance
//! now hits `bind_listener`'s "already accepting" bail immediately, before
//! it ever opens its own log.
//!
//! The event-log read, optional DuckDB rebuild, and session resume all move
//! to a background task ([`spawn_resume_task`]) that races the accept loop.
//! `hello`/`ping` never touch session state, so they're answered
//! immediately regardless of whether that background work has finished;
//! `session_list`/`session_load` would return an incomplete (or, right
//! after bind, empty) view of history if answered too early, so they block
//! on [`session::AgentdState::wait_until_resume_ready`] first -- a
//! readiness gate, not a protocol change.

mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::{self, AgentConfig};
use horizon_agent::contract::ProviderRegistry;
use horizon_agent::persistence::event_log::{Record, WriterHandle, WriterInit};
use horizon_agent::persistence::projection::duckdb::Store;
use horizon_agent::socket::default_socket_path;
use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, Hello, CONTRACT_VERSION};
use session::{AgentdState, Connection};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{
    unix::{OwnedReadHalf, OwnedWriteHalf},
    UnixListener, UnixStream,
};

/// Reported in this binary's `hello` reply's `binary_id` -- the crate
/// version, not the semantic "contract version" ([`CONTRACT_VERSION`],
/// carried separately in the same [`Hello`]).
const BINARY_ID: &str = env!("CARGO_PKG_VERSION");

/// Test-only hook (`crates/horizon-agentd/tests/e2e.rs`): when set to a
/// number of milliseconds, [`spawn_resume_task`] sleeps that long before
/// opening the event log, so a test can prove the bind-first ordering
/// (hello answers well before this delay elapses; `session_list`/
/// `session_load` don't) instead of relying on incidental timing. Never set
/// in production.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_RESUME_DELAY_MS";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path =
        socket_path_from_args(std::env::args().skip(1)).unwrap_or_else(default_socket_path);

    let file_config = config::load_file_config();
    let agent_config = AgentConfig::from_env_and_file(&file_config);
    eprintln!(
        "horizon-agentd: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    // Bind-first (see the module doc): this is the first thing that touches
    // the socket path, and it happens before the event log is even opened.
    let listener = bind_listener(&socket_path).await?;

    let providers = ProviderRegistry::builtin_with_config(agent_config.clone());
    let state = Arc::new(AgentdState::new(providers, agent_config.clone(), None));

    spawn_resume_task(state.clone(), agent_config);

    run(listener, &socket_path, state).await
}

/// Opens this process's own event log writer, resumes every session found
/// in it, and marks `state` ready -- all on a background task that races
/// the accept loop `main` starts right after this returns, per the
/// module's bind-first fix. Decision 3 in the design doc ("the child owns
/// the event log and DuckDB projection") still holds: this uses this
/// binary's own config load, never Horizon's.
fn spawn_resume_task(state: Arc<AgentdState>, agent_config: AgentConfig) {
    tokio::task::spawn_blocking(move || {
        if let Some(delay) = test_resume_delay() {
            std::thread::sleep(delay);
        }
        let (writer, records) = open_persistence(&agent_config);
        state.set_writer(writer);
        // Step 4: "agentd restart = read own log, rebuild rig_history, mark
        // turns that died mid-flight as cancelled ... sessions are live
        // again".
        session::resume_persisted_sessions(&state, records);
        state.mark_resume_ready();
    });
}

fn test_resume_delay() -> Option<Duration> {
    std::env::var(TEST_RESUME_DELAY_MS_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Opens this process's own event log writer and (if configured) rebuilds
/// the DuckDB projection from it, using this binary's own config load
/// (never Horizon's). Blocks the calling (background, per
/// [`spawn_resume_task`]) task on the writer's startup read -- simpler than
/// threading the `WriterInit` channel further, and no longer something the
/// accept loop waits on (see the module doc). Also hands back the startup
/// read's records (empty if persistence is disabled or there's nothing yet)
/// so the caller can resume every session they belong to
/// ([`session::resume_persisted_sessions`]).
///
/// Opens via [`WriterHandle::open_silently`] rather than [`WriterHandle::
/// open`]: this function already prints its own `horizon-agentd`-prefixed
/// skipped-lines summary just below, so the shared writer module's own
/// generic summary line would otherwise double up in agentd's stderr.
fn open_persistence(agent_config: &AgentConfig) -> (Option<WriterHandle>, Vec<Record>) {
    let (writer, init_rx) = WriterHandle::open_silently(&agent_config.persistence.event_log_path);
    match init_rx.recv() {
        Ok(WriterInit::Ready(report)) => {
            if let Some(summary) = report.skipped_summary() {
                eprintln!(
                    "horizon-agentd: {summary} while opening {}",
                    agent_config.persistence.event_log_path.display()
                );
            }
            if let Some(duckdb_path) = &agent_config.persistence.duckdb_path {
                if let Err(error) = rebuild_duckdb_projection(duckdb_path, report.records.clone()) {
                    eprintln!("horizon-agentd: DuckDB projection rebuild failed: {error}");
                }
            }
            (Some(writer), report.records)
        }
        Ok(WriterInit::Failed(error)) => {
            eprintln!(
                "horizon-agentd: event log unavailable ({error}); persistence disabled for this run"
            );
            (None, Vec::new())
        }
        Err(_) => {
            eprintln!(
                "horizon-agentd: event log writer thread exited before reporting startup status; \
                 persistence disabled for this run"
            );
            (None, Vec::new())
        }
    }
}

fn rebuild_duckdb_projection(path: &Path, records: Vec<Record>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let store = Store::open(path)?;
    store.replace_from_event_log_records(records)?;
    Ok(())
}

async fn run(
    listener: UnixListener,
    socket_path: &Path,
    state: Arc<AgentdState>,
) -> anyhow::Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = handle_connection(stream, state.clone()).await {
                    eprintln!("horizon-agentd: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-agentd: SIGTERM received, shutting down");
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
/// here"). Two phases: a fully sequential handshake (identical to step 2 --
/// must succeed before any concurrency starts, so a rejected/mismatched
/// hello can reply and close deterministically without racing an
/// independent writer task), then [`run_session_hosting_loop`], which needs
/// genuine read/write concurrency since a hosted session can push events at
/// any time, not just in reply to something Horizon sent.
async fn handle_connection(stream: UnixStream, state: Arc<AgentdState>) -> anyhow::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let envelope = match wire::read_envelope(&mut reader).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return Ok(()),
            Err(err) => {
                eprintln!("horizon-agentd: malformed message, closing connection: {err}");
                return Ok(());
            }
        };

        let EnvelopeBody::Control(control) = envelope.body else {
            eprintln!("horizon-agentd: command/event received before hello, ignoring");
            continue;
        };

        match control {
            Control::Hello(hello) => {
                if hello.contract_version != CONTRACT_VERSION {
                    let reason = format!(
                        "contract version mismatch: horizon-agentd speaks v{CONTRACT_VERSION}, \
                         client sent v{} -- reload required",
                        hello.contract_version
                    );
                    eprintln!("horizon-agentd: rejecting handshake: {reason}");
                    let _ = wire::write_envelope(
                        &mut writer,
                        &Envelope::control(Control::HandshakeRejected(reason)),
                    )
                    .await;
                    return Ok(());
                }
                wire::write_envelope(&mut writer, &our_hello_envelope()).await?;
                break;
            }
            Control::Ping => {
                wire::write_envelope(&mut writer, &Envelope::control(Control::Pong)).await?;
            }
            Control::SessionList => {
                wire::write_envelope(
                    &mut writer,
                    &Envelope::control(Control::SessionListResult(Vec::new())),
                )
                .await?;
            }
            Control::Drain => {
                let _ = writer.flush().await;
                eprintln!("horizon-agentd: drained, exiting");
                std::process::exit(0);
            }
            other => {
                eprintln!("horizon-agentd: {other:?} received before hello, ignoring");
            }
        }
    }

    run_session_hosting_loop(reader, writer, state).await
}

/// The post-handshake phase: a writer task owns the socket's write half and
/// drains an outgoing-envelope queue (fed by both this loop's own replies
/// and every hosted session's thread -- see [`Connection`]), while this
/// function keeps reading incoming envelopes and routing them. Splitting
/// read and write this way is what lets a session push events (or a
/// `host_tool_request`) to Horizon asynchronously, not just in response to
/// something Horizon just sent.
///
/// The writer task is deliberately never awaited to completion here: on a
/// normal disconnect, session threads spawned by [`Connection`] may still
/// hold `outgoing` senders (sessions outlive the connection that created
/// them within this process -- see `session`'s module doc), so the channel
/// would never close and awaiting the writer task would hang this function
/// forever, wedging the accept loop against ever serving a next connection.
/// Letting it run detached means it simply keeps trying to write to a dead
/// socket until its next send fails, at which point it exits on its own.
async fn run_session_hosting_loop(
    mut reader: BufReader<OwnedReadHalf>,
    mut writer: OwnedWriteHalf,
    state: Arc<AgentdState>,
) -> anyhow::Result<()> {
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
    let connection = Connection::new(outgoing_tx.clone(), state);

    tokio::spawn(async move {
        while let Some(envelope) = outgoing_rx.recv().await {
            if wire::write_envelope(&mut writer, &envelope).await.is_err() {
                break;
            }
        }
    });

    loop {
        let envelope = match wire::read_envelope(&mut reader).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => {
                connection.disconnect();
                return Ok(());
            }
            Err(err) => {
                eprintln!("horizon-agentd: malformed message, closing connection: {err}");
                connection.disconnect();
                return Ok(());
            }
        };

        match envelope.body {
            EnvelopeBody::Control(Control::Ping) => {
                let _ = outgoing_tx.send(Envelope::control(Control::Pong));
            }
            EnvelopeBody::Control(Control::SessionList) => {
                // Bind-first fix: block until `resume_persisted_sessions`
                // has finished, so a client that connects while it's still
                // running doesn't see an incomplete (or empty) session
                // list -- see the module doc and `AgentdState::
                // wait_until_resume_ready`.
                connection.wait_until_resume_ready().await;
                let _ = outgoing_tx.send(Envelope::control(Control::SessionListResult(
                    connection.session_list(),
                )));
            }
            EnvelopeBody::Control(Control::SessionNew(new)) => {
                connection.handle_session_new(new);
            }
            EnvelopeBody::Control(Control::SessionLoad(load)) => {
                // Same readiness gate as `SessionList` above: a resumed
                // session's thread may not exist yet while resume is still
                // in flight, which would otherwise make this replay as
                // "unknown session" (empty) instead of waiting for it.
                connection.wait_until_resume_ready().await;
                // Step 4's "v1 bootstrap": re-emit the fold-relevant
                // committed events for this session so the (re)connecting
                // client can rebuild its frame. Awaited inline (not spawned
                // detached) so these arrive before whatever the client sends
                // next for this session, keeping replay ordering simple.
                let events = connection.replay_events(load.session_id).await;
                for event in events {
                    let _ = outgoing_tx.send(Envelope::event(load.session_id, event));
                }
            }
            EnvelopeBody::Control(Control::HostToolResponse(response)) => {
                connection.handle_host_tool_response(response);
            }
            EnvelopeBody::Control(Control::Drain) => {
                eprintln!("horizon-agentd: drained, exiting");
                std::process::exit(0);
            }
            EnvelopeBody::Command(command) => match envelope.session_id {
                Some(session_id) => connection.route_command(session_id, command),
                None => eprintln!("horizon-agentd: command envelope missing session_id, ignoring"),
            },
            other => {
                eprintln!("horizon-agentd: unexpected message during session hosting: {other:?}");
            }
        }
    }
}

fn our_hello_envelope() -> Envelope {
    Envelope::control(Control::Hello(Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: BINARY_ID.to_string(),
        capabilities: vec!["sessions".to_string()],
    }))
}

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// `horizon-agentd` that didn't shut down cleanly), remove it and rebind.
/// If something *is* accepting, refuses to steal the path out from under a
/// live instance.
async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another horizon-agentd running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "horizon-agentd: removing stale socket {} (nothing was accepting)",
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
