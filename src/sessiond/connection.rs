use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use horizon_agent::contract::{self, Command};
use horizon_agent::wire::{self, HostToolResponse};
use horizon_session_protocol::{
    legacy, ClientHello, HubError, HubHello, SessionHub as _, SessionHubClient,
    TerminalAttachment, WireCodec, SESSION_PROTOCOL_VERSION,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use remoc::rch;
use remoc::rtc::Client as _;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::routing::Routes;

/// The bound on the whole remoc establishment sequence — chmux handshake,
/// base-channel handover, and the `hello` rtc call. A healthy v10 daemon
/// completes it in milliseconds (its accept loop serves the hub before it
/// even opens its event log); what this bounds is the *cross-generation*
/// case (`docs/remoc-adoption-design.md` §6): a still-running JSONL
/// daemon reads our chmux hello as a torn JSON line and either closes
/// (fast transport error) or keeps waiting for a newline that never comes
/// (this timeout) — never chmux's own raw 60 s `ChMux(Timeout)`.
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct RuntimeControl {
    cancelled: AtomicBool,
    established: AtomicBool,
    notify: Notify,
    stopped: (Mutex<bool>, Condvar),
}

impl RuntimeControl {
    pub(super) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            established: AtomicBool::new(false),
            notify: Notify::new(),
            stopped: (Mutex::new(false), Condvar::new()),
        }
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(super) fn is_established(&self) -> bool {
        self.established.load(Ordering::Acquire)
    }

    pub(super) fn wait_stopped(&self) {
        let (lock, wake) = &self.stopped;
        let mut stopped = lock.lock().unwrap();
        while !*stopped {
            stopped = wake.wait(stopped).unwrap();
        }
    }

    async fn cancelled(&self) {
        let notified = self.notify.notified();
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    fn mark_established(&self) {
        self.established.store(true, Ordering::Release);
    }

    fn mark_stopped(&self) {
        let (lock, wake) = &self.stopped;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}

/// One typed request from the sync world to the runtime — the v10
/// replacement for the raw-envelope FIFO. Requests that used to need a
/// `request_id` correlation map carry their reply channel directly; the
/// command streams carry the receiving half of their handle's bridge.
pub(super) enum Op {
    NewAgent {
        new: wire::SessionNew,
        commands: UnboundedReceiver<Command>,
    },
    AttachAgent {
        session_id: contract::SessionId,
        commands: UnboundedReceiver<Command>,
    },
    CreateTerminal {
        session_id: Uuid,
        spec: Box<TerminalSpawnSpec>,
        commands: UnboundedReceiver<TerminalCommand>,
    },
    AttachTerminal {
        session_id: Uuid,
        commands: UnboundedReceiver<TerminalCommand>,
        /// `true` exactly when the daemon reported a successful attach.
        reply: crossbeam_channel::Sender<bool>,
    },
    TerminalList {
        reply: crossbeam_channel::Sender<Result<Vec<TerminalSummary>, String>>,
    },
    SessionList {
        reply: crossbeam_channel::Sender<Result<Vec<wire::SessionSummary>, String>>,
    },
    HostToolResponse(HostToolResponse),
    Drain,
}

pub(super) fn spawn(
    socket_path: PathBuf,
    control_socket: PathBuf,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<Routes>,
    control: Arc<RuntimeControl>,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                routes.connection_failed(format!(
                    "could not start the horizon-sessiond client runtime: {error}"
                ));
                control.mark_stopped();
                return;
            }
        };
        runtime.block_on(async {
            let mut mismatch_recovery_attempted = false;
            loop {
                let stream = tokio::select! {
                    result = horizon_agent::client::connect_or_spawn_retrying(
                        &socket_path,
                        &control_socket,
                    ) => match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("horizon-sessiond initial connection failed: {error}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    },
                    _ = control.cancelled() => break,
                };

                match run_stream(stream, &mut ops, routes.clone(), control.clone()).await {
                    StreamEnd::GenerationMismatch { message } => {
                        // Auto-recovery across the transport generation
                        // (docs/remoc-adoption-design.md §6, extending PR
                        // #18's decisions): drain the stale JSONL daemon at
                        // *its own* envelope version via the quarantined
                        // legacy encoder, then let the next iteration's
                        // connect_or_spawn_retrying start a fresh binary.
                        // Attempted exactly once per runtime: if the
                        // respawned daemon still can't speak remoc (a stale
                        // horizon-sessiond binary -- `cargo run` rebuilds
                        // only the horizon binary), restarting it again
                        // would loop forever, so give up loudly instead.
                        if mismatch_recovery_attempted {
                            let error = format!(
                                "{message} -- automatic drain-and-restart was already attempted \
                                 once; rebuild horizon-sessiond (`cargo build --workspace`) and \
                                 run `Reload Session Runtime`"
                            );
                            eprintln!("horizon-sessiond connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                        mismatch_recovery_attempted = true;
                        eprintln!(
                            "a horizon-sessiond that does not speak the v{SESSION_PROTOCOL_VERSION} \
                             remoc wire detected ({message}); draining and restarting it"
                        );
                        let drained = tokio::select! {
                            drained = drain_stale_sessiond(&socket_path) => drained,
                            _ = control.cancelled() => {
                                routes.connection_failed("sessiond runtime stopped".to_string());
                                break;
                            }
                        };
                        if let Err(error) = drained {
                            let error =
                                format!("{message} -- and the automatic drain failed: {error}");
                            eprintln!("horizon-sessiond connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                    }
                    StreamEnd::VersionRejected { message } => {
                        // A healthy remoc daemon whose negotiated range
                        // doesn't overlap ours -- the successor of the JSONL
                        // `HandshakeRejected` recovery: ask it to drain over
                        // a fresh hub connection, once per runtime.
                        if mismatch_recovery_attempted {
                            let error = format!(
                                "{message} -- automatic drain-and-restart was already attempted \
                                 once; rebuild horizon-sessiond (`cargo build --workspace`) and \
                                 run `Reload Session Runtime`"
                            );
                            eprintln!("horizon-sessiond connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                        mismatch_recovery_attempted = true;
                        eprintln!("{message}; draining and restarting the daemon");
                        let drained = tokio::select! {
                            drained = drain_incompatible_remoc_sessiond(&socket_path) => drained,
                            _ = control.cancelled() => {
                                routes.connection_failed("sessiond runtime stopped".to_string());
                                break;
                            }
                        };
                        if let Err(error) = drained {
                            let error =
                                format!("{message} -- and the automatic drain failed: {error}");
                            eprintln!("horizon-sessiond connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                    }
                    StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                        eprintln!("horizon-sessiond connection stopped: {error}");
                        routes.connection_failed(error);
                        break;
                    }
                    StreamEnd::Cancelled => {
                        routes.connection_failed("sessiond runtime stopped".to_string());
                        break;
                    }
                    StreamEnd::Dropped => break,
                }
            }
        });
        control.mark_stopped();
    });
}

#[cfg(test)]
pub(super) fn spawn_test_stream<S>(
    stream: S,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<Routes>,
    control: Arc<RuntimeControl>,
) where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let end = runtime.block_on(run_stream(
            stream,
            &mut ops,
            routes.clone(),
            control.clone(),
        ));
        match end {
            StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                routes.connection_failed(error)
            }
            // Mismatch recovery needs a real socket to drain and a daemon
            // to respawn; a test stream has neither, so the mismatch
            // surfaces as a terminal failure instead.
            StreamEnd::GenerationMismatch { message } | StreamEnd::VersionRejected { message } => {
                routes.connection_failed(message)
            }
            StreamEnd::Cancelled | StreamEnd::Dropped => {}
        }
        control.mark_stopped();
    });
}

enum StreamEnd {
    /// The peer on the socket never completed a remoc handshake + hello:
    /// either the transport failed during it, or [`ESTABLISH_TIMEOUT`]
    /// elapsed. This is how a still-running JSONL-generation (v≤9) daemon
    /// presents -- it cannot decode chmux at all, so it can neither answer
    /// nor say which version it speaks. A daemon that genuinely died in
    /// this window looks the same, but the recovery path is harmless there
    /// (its socket already refuses connections, so the drain is a no-op
    /// and the respawn is exactly what a dead daemon needs). Recoverable:
    /// see `spawn`'s probe-drain-restart arm.
    GenerationMismatch { message: String },
    /// The daemon speaks remoc and answered `hello` with an explicit
    /// version-range rejection. Recoverable via an rtc `drain`.
    VersionRejected { message: String },
    Fatal(String),
    EstablishedFailure(String),
    Cancelled,
    Dropped,
}

/// What a successful establishment hands the op loop.
struct Live {
    hub: SessionHubClient<WireCodec>,
    host_tool_responses: rch::mpsc::Sender<HostToolResponse, WireCodec>,
    routes: Arc<Routes>,
}

async fn run_stream<S>(
    stream: S,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<Routes>,
    control: Arc<RuntimeControl>,
) -> StreamEnd
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let established = tokio::select! {
        result = establish(stream) => result,
        _ = control.cancelled() => return StreamEnd::Cancelled,
    };
    let (hub, hello, conn_task) = match established {
        Ok(established) => established,
        Err(EstablishError::NoRemocPeer(message)) => {
            return StreamEnd::GenerationMismatch { message }
        }
        Err(EstablishError::Rejected(message)) => return StreamEnd::VersionRejected { message },
        Err(EstablishError::Fatal(message)) => return StreamEnd::Fatal(message),
    };
    control.mark_established();

    let HubHello {
        negotiated: _,
        binary_id: _,
        host_tools,
        host_tool_responses,
        skipped_lines,
    } = hello;

    // Connection-global inbound pumps.
    spawn_host_tool_pump(host_tools, routes.clone());
    spawn_skipped_lines_pump(skipped_lines);

    let live = Live {
        hub: hub.clone(),
        host_tool_responses,
        routes: routes.clone(),
    };

    // `closed` completes when the server side (or the connection) is gone
    // -- the uniform disconnect signal every channel shares now.
    let mut closed = hub.closed();
    let end = loop {
        tokio::select! {
            _ = control.cancelled() => break StreamEnd::Cancelled,
            _ = &mut closed => {
                break StreamEnd::EstablishedFailure(
                    "established sessiond disconnected".to_string(),
                );
            }
            op = ops.recv() => {
                let Some(op) = op else {
                    break StreamEnd::Dropped;
                };
                handle_op(op, &live);
            }
        }
    };
    conn_task.abort();
    end
}

enum EstablishError {
    /// No remoc endpoint on the other side (transport error, closed, or
    /// timed out mid-handshake) — the generation-mismatch signal.
    NoRemocPeer(String),
    /// The daemon's hub rejected our version range.
    Rejected(String),
    Fatal(String),
}

type EstablishedParts = (
    SessionHubClient<WireCodec>,
    HubHello,
    JoinHandle<Result<(), remoc::chmux::ChMuxError<std::io::Error, std::io::Error>>>,
);

/// Runs the remoc connect + base handover + `hello`, each leg bounded by
/// one shared [`ESTABLISH_TIMEOUT`] deadline. The chmux multiplexer task
/// is spawned as soon as the connect completes (adoption condition 3 — it
/// must be polled concurrently with everything that follows) and is
/// aborted before returning on every failure path, so a timed-out
/// establishment never leaks a task still holding the socket.
async fn establish<S>(stream: S) -> Result<EstablishedParts, EstablishError>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let deadline = tokio::time::Instant::now() + ESTABLISH_TIMEOUT;
    let (read_half, write_half) = tokio::io::split(stream);

    let connect =
        remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        );
    let (conn, _base_tx, mut base_rx) = match tokio::time::timeout_at(deadline, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => {
            return Err(EstablishError::NoRemocPeer(format!(
                "sessiond did not complete a remoc handshake (likely a stale pre-v10 JSONL \
                 daemon): {error}"
            )))
        }
        Err(_elapsed) => {
            return Err(EstablishError::NoRemocPeer(format!(
                "sessiond sent no remoc handshake within {ESTABLISH_TIMEOUT:?} (likely a stale \
                 pre-v10 JSONL daemon)"
            )))
        }
    };
    let conn_task = tokio::spawn(conn);

    let hub = match tokio::time::timeout_at(deadline, base_rx.recv()).await {
        Ok(Ok(Some(hub))) => hub,
        Ok(Ok(None)) | Ok(Err(_)) => {
            conn_task.abort();
            return Err(EstablishError::NoRemocPeer(
                "sessiond closed the connection before handing over its hub client".to_string(),
            ));
        }
        Err(_elapsed) => {
            conn_task.abort();
            return Err(EstablishError::NoRemocPeer(format!(
                "sessiond handed over no hub client within {ESTABLISH_TIMEOUT:?}"
            )));
        }
    };

    let client_hello = ClientHello::new(concat!("horizon/", env!("CARGO_PKG_VERSION")));
    match tokio::time::timeout_at(deadline, hub.hello(client_hello)).await {
        Ok(Ok(hello)) => Ok((hub, hello, conn_task)),
        Ok(Err(error @ HubError::IncompatibleVersion { .. })) => {
            conn_task.abort();
            Err(EstablishError::Rejected(format!(
                "sessiond rejected the handshake: {error}"
            )))
        }
        Ok(Err(error)) => {
            conn_task.abort();
            Err(EstablishError::Fatal(format!(
                "sessiond answered hello with an unexpected error: {error}"
            )))
        }
        Err(_elapsed) => {
            conn_task.abort();
            Err(EstablishError::NoRemocPeer(format!(
                "sessiond did not answer hello within {ESTABLISH_TIMEOUT:?}"
            )))
        }
    }
}

fn spawn_host_tool_pump(
    mut host_tools: rch::mpsc::Receiver<wire::HostToolRequest, WireCodec>,
    routes: Arc<Routes>,
) {
    tokio::spawn(async move {
        loop {
            match host_tools.recv().await {
                Ok(Some(request)) => routes.host_tool_request(request),
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                // Adoption condition 2: skip the poisoned item, keep the
                // channel.
                Err(err) => {
                    eprintln!("horizon-sessiond sent an undecodable host-tool request: {err}")
                }
            }
        }
    });
}

fn spawn_skipped_lines_pump(mut skipped_lines: rch::mpsc::Receiver<String, WireCodec>) {
    tokio::spawn(async move {
        while let Ok(Some(summary)) = skipped_lines.recv().await {
            // No pane consumes this today (parity with the JSONL wire,
            // where the control was routed and then dropped); surfacing it
            // in the log keeps the diagnostic visible.
            eprintln!("horizon-sessiond event log: {summary}");
        }
    });
}

/// Dispatches one op. Every rtc call runs on its own task (the calls are
/// independent and a slow one — a PTY spawn, a large replay — must not
/// stall command forwarding for other sessions), holding clones of the
/// hub client and routes.
fn handle_op(op: Op, live: &Live) {
    match op {
        Op::NewAgent { new, commands } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                let session_id = new.session_id;
                match hub.new_agent(new).await {
                    Ok(attachment) => {
                        run_agent_attachment(routes, session_id, attachment, commands).await
                    }
                    Err(error) => routes.agent_failed(
                        session_id,
                        format!("failed to start the agent session: {error}"),
                    ),
                }
            });
        }
        Op::AttachAgent {
            session_id,
            commands,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match hub.attach_agent(session_id).await {
                    Ok(attachment) => {
                        run_agent_attachment(routes, session_id, attachment, commands).await
                    }
                    Err(error) => routes.agent_failed(
                        session_id,
                        format!("failed to attach to the agent session: {error}"),
                    ),
                }
            });
        }
        Op::CreateTerminal {
            session_id,
            spec,
            commands,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match hub.create_terminal(session_id, *spec).await {
                    Ok(attachment) => {
                        run_terminal_attachment(routes, session_id, attachment, commands).await
                    }
                    // What the JSONL wire delivered as a
                    // `TerminalUpdate::Error` on the update stream.
                    Err(error) => routes.terminal_failed(session_id, error.to_string()),
                }
            });
        }
        Op::AttachTerminal {
            session_id,
            commands,
            reply,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match hub.attach_terminal(session_id).await {
                    Ok(attachment) => {
                        let _ = reply.send(true);
                        run_terminal_attachment(routes, session_id, attachment, commands).await;
                    }
                    Err(_error) => {
                        let _ = reply.send(false);
                    }
                }
            });
        }
        Op::TerminalList { reply } => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                let result = hub
                    .list_terminals()
                    .await
                    .map_err(|error| format!("terminal list failed: {error}"));
                let _ = reply.send(result);
            });
        }
        Op::SessionList { reply } => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                let result = hub
                    .list_agents()
                    .await
                    .map_err(|error| format!("agent list failed: {error}"));
                let _ = reply.send(result);
            });
        }
        Op::HostToolResponse(response) => {
            let sender = live.host_tool_responses.clone();
            tokio::spawn(async move {
                let _ = sender.send(response).await;
            });
        }
        Op::Drain => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                // The daemon exits inside this call, so the reply usually
                // never arrives; completion is observed by the caller as
                // the socket refusing connections (`wait_for_drain`).
                let _ = hub.drain().await;
            });
        }
    }
}

/// One live terminal attachment: forwards handle commands to the daemon
/// and routes daemon updates to the pane, until either side goes away.
async fn run_terminal_attachment(
    routes: Arc<Routes>,
    session_id: Uuid,
    attachment: TerminalAttachment,
    mut commands: UnboundedReceiver<TerminalCommand>,
) {
    let TerminalAttachment {
        mut updates,
        commands: remote_commands,
    } = attachment;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if remote_commands.send(command).await.is_err() {
                        break;
                    }
                }
                // The pane's handle (and its bridge thread) are gone.
                None => break,
            },
            update = updates.recv() => match update {
                Ok(Some(update)) => {
                    let exited = matches!(update, TerminalUpdate::Exited);
                    routes.route_terminal_update(session_id, update);
                    if exited {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                // Adoption condition 2: one undecodable update is skipped;
                // the channel survives. A degraded frame lasts until the
                // next one replaces it.
                Err(err) => {
                    eprintln!("skipping an undecodable terminal update for {session_id}: {err}")
                }
            },
        }
    }
}

/// One live agent attachment — the agent-domain twin of
/// [`run_terminal_attachment`].
async fn run_agent_attachment(
    routes: Arc<Routes>,
    session_id: contract::SessionId,
    attachment: horizon_session_protocol::AgentAttachment,
    mut commands: UnboundedReceiver<Command>,
) {
    let horizon_session_protocol::AgentAttachment {
        mut events,
        commands: remote_commands,
    } = attachment;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if remote_commands.send(command).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            event = events.recv() => match event {
                Ok(Some(event)) => routes.route_agent_event(session_id, event),
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                Err(err) => {
                    eprintln!("skipping an undecodable agent event for {session_id:?}: {err}")
                }
            },
        }
    }
}

/// Per-probe budget for a drained daemon's process to actually exit,
/// observed as its socket refusing connections -- the same signal (and the
/// same 2s budget) as `super::wait_for_drain`, which the explicit `Reload
/// Session Runtime` flow uses.
const DRAIN_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Gracefully stops a JSONL-generation daemon by sending it a
/// `session_control` `Drain` *at its own envelope version* on a fresh
/// connection, via the quarantined legacy encoder
/// (`horizon_session_protocol::legacy` — the sole surviving JSONL code
/// path, and this function is its only caller). A v≤9 daemon never
/// reveals its version to a v10 client (it cannot decode chmux at all),
/// so this always probes downward from the newest JSONL version; a probe
/// at the wrong version is harmless (the daemon logs a malformed message
/// and closes that one connection), and a probe at the right one drains
/// it. A wrong-generation probe against a healthy remoc daemon is equally
/// harmless: the line is chmux garbage, the daemon's handshake timeout
/// drops that one connection.
async fn drain_stale_sessiond(socket_path: &Path) -> Result<(), String> {
    for version in
        (legacy::OLDEST_DRAINABLE_VERSION..=legacy::NEWEST_JSONL_VERSION).rev()
    {
        let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(stream) => stream,
            // Nothing is accepting any more: either a previous probe's
            // drain just landed or the daemon died on its own. Done either
            // way -- the caller's next connect_or_spawn_retrying starts a
            // fresh daemon.
            Err(_) => return Ok(()),
        };
        if stream
            .write_all(legacy::drain_line(version).as_bytes())
            .await
            .is_err()
        {
            continue;
        }
        let _ = stream.flush().await;
        drop(stream);
        if wait_until_refusing(socket_path).await {
            return Ok(());
        }
    }
    Err(
        "horizon-sessiond kept accepting connections after every drain probe; \
         stop it manually"
            .to_string(),
    )
}

/// Gracefully stops a remoc daemon whose version range doesn't overlap
/// ours: `hello` and `drain` are the version-stable hub surface, so the
/// drain travels as an ordinary rtc call on a fresh connection.
async fn drain_incompatible_remoc_sessiond(socket_path: &Path) -> Result<(), String> {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    match establish_for_drain(stream).await {
        Ok((hub, conn_task)) => {
            let _ = hub.drain().await;
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("drain connection to the incompatible sessiond failed: {error}");
        }
    }
    if wait_until_refusing(socket_path).await {
        Ok(())
    } else {
        Err("horizon-sessiond kept accepting connections after the drain call; \
             stop it manually"
            .to_string())
    }
}

/// A minimal establish for the drain path: connect + base handover only —
/// no `hello`, since the whole point is that `hello` already failed.
async fn establish_for_drain(
    stream: tokio::net::UnixStream,
) -> Result<
    (
        SessionHubClient<WireCodec>,
        JoinHandle<Result<(), remoc::chmux::ChMuxError<std::io::Error, std::io::Error>>>,
    ),
    String,
> {
    let deadline = tokio::time::Instant::now() + ESTABLISH_TIMEOUT;
    let (read_half, write_half) = stream.into_split();
    let connect = remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, _base_tx, mut base_rx) = tokio::time::timeout_at(deadline, connect)
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let conn_task = tokio::spawn(conn);
    match tokio::time::timeout_at(deadline, base_rx.recv()).await {
        Ok(Ok(Some(hub))) => Ok((hub, conn_task)),
        other => {
            conn_task.abort();
            Err(format!("no hub client handed over: {other:?}"))
        }
    }
}

/// True once `socket_path` refuses connections (the daemon process is
/// gone -- its drain exit leaves the socket file behind, so file
/// existence proves nothing); false if it still accepts when
/// [`DRAIN_EXIT_TIMEOUT`] runs out.
async fn wait_until_refusing(socket_path: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + DRAIN_EXIT_TIMEOUT;
    loop {
        if tokio::net::UnixStream::connect(socket_path).await.is_err() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}
