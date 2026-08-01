//! The `horizon-agentd` (agent runtime) client connection: connect,
//! negotiate, dispatch agent ops, recover from a stale daemon generation.
//!
//! Terminal traffic left this module in v17 — it has its own connection to
//! its own daemon in [`super::terminal`] (`docs/terminald-split-design.md`).
//! What is left here is exactly the agent domain plus the connection-global
//! host-tool exchange, which means a `Drain` sent from here can no longer
//! take a single PTY with it.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{self, Command};
use horizon_agent::wire::{
    self, agent_client_hello, HostToolResponse, HubHello, SessionHub as _, SessionHubClient,
    AGENT_PROTOCOL_VERSION,
};
use horizon_wire::{
    CappedReceiver, DecodeSkipLog, HubError, WireCodec, CONTROL_MAX_ITEM_BYTES,
    TOOL_IO_MAX_ITEM_BYTES,
};
use remoc::rch;
use remoc::rtc::Client as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;

use super::common::{
    connect_hub, establish_timeout, wait_until_refusing, with_deadline, ConnTask, Connected,
    EstablishError, RuntimeControl, StreamEnd, OP_TIMEOUT, SILENCE_MISMATCH_THRESHOLD,
};
use super::routing::AgentRoutes;

/// The daemon this module talks to, named in every classified error.
const DAEMON: &str = "horizon-agentd";

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
    SessionList {
        reply: crossbeam_channel::Sender<Result<Vec<wire::SessionSummary>, String>>,
    },
    HostToolResponse(HostToolResponse),
    Drain,
    /// Fire-and-forget request to rebuild `[provider]` in the running
    /// daemon without a respawn -- see [`SessionHub::reload_provider_config`].
    ReloadProviderConfig,
}

pub(super) fn spawn(
    socket_path: PathBuf,
    control_socket: PathBuf,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
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
                    "could not start the horizon-agentd client runtime: {error}"
                ));
                control.mark_stopped();
                return;
            }
        };
        runtime.block_on(async {
            let mut mismatch_recovery_attempted = false;
            // The transient-retry backoff (the JSONL era's
            // `hello_retry_delay`, restored) and the consecutive-silence
            // counter behind `SILENCE_MISMATCH_THRESHOLD`.
            let mut retry_delay = Duration::from_millis(50);
            let mut consecutive_silences: u32 = 0;
            loop {
                let stream = tokio::select! {
                    result = horizon_wire::spawn::connect_or_spawn_agentd_retrying(
                        &socket_path,
                        &control_socket,
                    ) => match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("horizon-agentd initial connection failed: {error}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    },
                    _ = control.cancelled() => break,
                };

                match run_stream(stream, &mut ops, routes.clone(), control.clone()).await {
                    StreamEnd::PreHelloTransport { message } => {
                        // Transient: retry with backoff, never consuming the
                        // recovery budget. A differently-shaped failure also
                        // breaks any "persistent silence" pattern.
                        consecutive_silences = 0;
                        eprintln!("horizon-agentd hello transport failed, retrying: {message}");
                        tokio::select! {
                            _ = tokio::time::sleep(retry_delay) => {}
                            _ = control.cancelled() => {
                                routes.connection_failed("agentd runtime stopped".to_string());
                                break;
                            }
                        }
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                        continue;
                    }
                    StreamEnd::Silence { message } => {
                        consecutive_silences += 1;
                        if consecutive_silences < SILENCE_MISMATCH_THRESHOLD {
                            // One silent deadline is not generation
                            // evidence (a busy daemon/host) -- retry.
                            eprintln!(
                                "horizon-agentd did not answer within the establish deadline \
                                 ({consecutive_silences}/{SILENCE_MISMATCH_THRESHOLD} before \
                                 mismatch recovery): {message}"
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(retry_delay) => {}
                                _ = control.cancelled() => {
                                    routes.connection_failed(
                                        "agentd runtime stopped".to_string(),
                                    );
                                    break;
                                }
                            }
                            retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                            continue;
                        }
                        // Persistent silence IS how a real JSONL daemon
                        // presents (docs/remoc-adoption-design.md par.6's
                        // bounded-timeout detection): fall through to the
                        // recovery arm below.
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_generation_mismatch(
                            &message,
                            &mut mismatch_recovery_attempted,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::GenerationMismatch { message } => {
                        // Positive garbage evidence goes straight to the
                        // recovery arm -- no healthy remoc daemon can send
                        // non-chmux bytes.
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_generation_mismatch(
                            &message,
                            &mut mismatch_recovery_attempted,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::VersionRejected { message } => {
                        consecutive_silences = 0;
                        // A healthy remoc daemon whose negotiated range
                        // doesn't overlap ours -- the successor of the JSONL
                        // `HandshakeRejected` recovery: ask it to drain over
                        // a fresh hub connection, once per runtime.
                        if mismatch_recovery_attempted {
                            let error = format!(
                                "{message} -- automatic drain-and-restart was already attempted \
                                 once; rebuild horizon-agentd (`cargo build --workspace`) and \
                                 run `Reload Agent Runtime`"
                            );
                            eprintln!("horizon-agentd connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                        mismatch_recovery_attempted = true;
                        eprintln!("{message}; draining and restarting the daemon");
                        let drained = tokio::select! {
                            drained = drain_stale_agentd(&socket_path) => drained,
                            _ = control.cancelled() => {
                                routes.connection_failed("agentd runtime stopped".to_string());
                                break;
                            }
                        };
                        if let Err(error) = drained {
                            let error =
                                format!("{message} -- and the automatic drain failed: {error}");
                            eprintln!("horizon-agentd connection stopped: {error}");
                            routes.connection_failed(error);
                            break;
                        }
                    }
                    StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                        eprintln!("horizon-agentd connection stopped: {error}");
                        routes.connection_failed(error);
                        break;
                    }
                    StreamEnd::Cancelled => {
                        routes.connection_failed("agentd runtime stopped".to_string());
                        break;
                    }
                    StreamEnd::Dropped => break,
                }
            }
        });
        control.mark_stopped();
    });
}

/// The once-per-runtime recovery for a daemon generation this build cannot
/// negotiate with (`docs/remoc-adoption-design.md` §6, extending PR #18's
/// decisions): drain it over the version-stable rtc surface, then let the
/// caller's next `connect_or_spawn_agentd_retrying` start a fresh binary.
/// `Break` means the runtime must stop (budget already spent, drain failed,
/// or cancelled) -- `connection_failed` has already been fanned out then;
/// `Continue` means recovery succeeded and the caller should reconnect.
///
/// A daemon too old to answer that rtc call at all (a pre-remoc, v≤9 binary
/// still holding the socket) is no longer recoverable automatically: the
/// JSONL drain prober that used to cover it was deleted on 2026-08-01, so
/// this path reports the failure and the user stops the process by hand.
async fn recover_generation_mismatch(
    message: &str,
    mismatch_recovery_attempted: &mut bool,
    socket_path: &Path,
    routes: &Arc<AgentRoutes>,
    control: &Arc<RuntimeControl>,
) -> ControlFlow<()> {
    if *mismatch_recovery_attempted {
        // If the respawned daemon still can't speak remoc (a stale
        // horizon-agentd binary -- `cargo run` rebuilds only the horizon
        // binary), restarting it again would loop forever, so give up
        // loudly instead.
        let error = format!(
            "{message} -- automatic drain-and-restart was already attempted \
             once; rebuild horizon-agentd (`cargo build --workspace`) and \
             run `Reload Agent Runtime`"
        );
        eprintln!("horizon-agentd connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    *mismatch_recovery_attempted = true;
    eprintln!(
        "a horizon-agentd that does not speak the v{AGENT_PROTOCOL_VERSION} \
         remoc wire detected ({message}); draining and restarting it"
    );
    let drained = tokio::select! {
        drained = drain_stale_agentd(socket_path) => drained,
        _ = control.cancelled() => {
            routes.connection_failed("agentd runtime stopped".to_string());
            return ControlFlow::Break(());
        }
    };
    if let Err(error) = drained {
        let error = format!("{message} -- and the automatic drain failed: {error}");
        eprintln!("horizon-agentd connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
pub(super) fn spawn_test_stream<S>(
    stream: S,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
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
            // A test stream cannot be re-dialed either, so a transient or
            // silent end just stops the runtime (like the JSONL era's
            // test-stream handling of `PreHelloTransport`).
            StreamEnd::PreHelloTransport { .. }
            | StreamEnd::Silence { .. }
            | StreamEnd::Cancelled
            | StreamEnd::Dropped => {}
        }
        control.mark_stopped();
    });
}

/// What a successful establishment hands the op loop.
struct Live {
    hub: SessionHubClient<WireCodec>,
    host_tool_responses: rch::mpsc::Sender<HostToolResponse, WireCodec>,
    routes: Arc<AgentRoutes>,
}

async fn run_stream<S>(
    stream: S,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<AgentRoutes>,
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
        Err(error) => return error.into(),
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
                    "established agentd disconnected".to_string(),
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

type EstablishedParts = (SessionHubClient<WireCodec>, HubHello, ConnTask);

/// Runs the shared connect prelude ([`connect_hub`]) and then this hub's own
/// `hello`, both legs bounded by the one establish deadline the prelude
/// opened.
async fn establish<S>(stream: S) -> Result<EstablishedParts, EstablishError>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let Connected {
        hub,
        conn_task,
        deadline,
        timeout,
    } = connect_hub::<_, SessionHubClient<WireCodec>>(stream, DAEMON).await?;

    let client_hello = agent_client_hello(concat!("horizon/", env!("CARGO_PKG_VERSION")));
    match tokio::time::timeout_at(deadline, hub.hello(client_hello)).await {
        Ok(Ok(hello)) => Ok((hub, hello, conn_task)),
        Ok(Err(error @ HubError::IncompatibleVersion { .. })) => {
            conn_task.abort();
            Err(EstablishError::Rejected(format!(
                "agentd rejected the handshake: {error}"
            )))
        }
        // The hello call's own transport failure — a connection drop
        // mid-call. Transient, like every other pre-hello drop (this used
        // to go fatal; the review fixed that regression).
        Ok(Err(error @ HubError::Call(_))) => {
            conn_task.abort();
            Err(EstablishError::Transient(format!(
                "the connection dropped during hello: {error}"
            )))
        }
        Ok(Err(error)) => {
            conn_task.abort();
            Err(EstablishError::Fatal(format!(
                "agentd answered hello with an unexpected error: {error}"
            )))
        }
        Err(_elapsed) => {
            conn_task.abort();
            Err(EstablishError::Silence(format!(
                "agentd did not answer hello within {timeout:?}"
            )))
        }
    }
}

fn spawn_host_tool_pump(
    mut host_tools: CappedReceiver<wire::HostToolRequest, TOOL_IO_MAX_ITEM_BYTES>,
    routes: Arc<AgentRoutes>,
) {
    tokio::spawn(async move {
        let mut skips = DecodeSkipLog::new("host-tool requests");
        loop {
            match host_tools.recv().await {
                Ok(Some(request)) => routes.host_tool_request(request),
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                // Adoption condition 2: skip the poisoned item, keep the
                // channel.
                Err(err) => skips.note(&err),
            }
        }
    });
}

fn spawn_skipped_lines_pump(mut skipped_lines: CappedReceiver<String, CONTROL_MAX_ITEM_BYTES>) {
    tokio::spawn(async move {
        while let Ok(Some(summary)) = skipped_lines.recv().await {
            // No pane consumes this today (parity with the JSONL wire,
            // where the control was routed and then dropped); surfacing it
            // in the log keeps the diagnostic visible.
            eprintln!("horizon-agentd event log: {summary}");
        }
    });
}

/// Dispatches one op. Every rtc call runs on its own task (the calls are
/// independent and a slow one — a large replay — must not stall command
/// forwarding for other sessions), holding clones of the hub client and
/// routes.
fn handle_op(op: Op, live: &Live) {
    match op {
        Op::NewAgent { new, commands } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                let session_id = new.session_id;
                match with_deadline(OP_TIMEOUT, "new_agent", hub.new_agent(new)).await {
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
                match with_deadline(OP_TIMEOUT, "attach_agent", hub.attach_agent(session_id)).await
                {
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
        Op::SessionList { reply } => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                let result = with_deadline(OP_TIMEOUT, "agent list", hub.list_agents()).await;
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
                // the socket refusing connections (`wait_for_drain`) --
                // bounded so an unresponsive daemon can't pin this task.
                let _ = tokio::time::timeout(establish_timeout(), hub.drain()).await;
            });
        }
        Op::ReloadProviderConfig => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                if let Err(error) = with_deadline(
                    OP_TIMEOUT,
                    "reload_provider_config",
                    hub.reload_provider_config(),
                )
                .await
                {
                    eprintln!("horizon-agentd client: provider config reload failed: {error}");
                }
            });
        }
    }
}

/// One live agent attachment: forwards handle commands to the daemon and
/// routes events to the pane, until either side goes away.
async fn run_agent_attachment(
    routes: Arc<AgentRoutes>,
    session_id: contract::SessionId,
    attachment: horizon_agent::wire::AgentAttachment,
    mut commands: UnboundedReceiver<Command>,
) {
    let horizon_agent::wire::AgentAttachment {
        mut events,
        commands: remote_commands,
    } = attachment;
    let mut event_skips = DecodeSkipLog::new("agent events");
    let mut command_skips = DecodeSkipLog::new("agent commands");
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if let Err(err) = remote_commands.send(command).await {
                        // rch latches remote-send errors on the sender (one
                        // failure means every later send fails too), so any
                        // send error ends the attachment rather than
                        // skip-looping.
                        command_skips.note(&err);
                        break;
                    }
                }
                None => break,
            },
            event = events.recv() => match event {
                Ok(Some(event)) => routes.route_agent_event(session_id, event),
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                Err(err) => event_skips.note(&err),
            },
        }
    }
}

/// Gracefully stops a running agentd this build cannot talk to: `hello` and
/// `drain` are the version-stable hub surface, so the drain travels as an
/// ordinary rtc call on a fresh connection.
///
/// It reaches every daemon that still speaks *some* remoc wire, which since
/// 2026-08-01 is the only generation this recovery covers: the pre-remoc
/// (JSONL) drain prober -- the last remnant of that era -- was deleted then,
/// so a v≤9 daemon still holding the socket falls into the same bucket as the
/// documented v16→v17 case below — the error is the honest outcome and names
/// the manual fix.
///
/// That v16→v17 case: the terminald split removed methods from the middle of
/// `SessionHub`, which shifts every later method's index under the
/// index-encoded request enum, so a *still-running v16 daemon* (the binary
/// then named `horizon-sessiond`) decodes this drain as a different method
/// and keeps accepting (see `AGENT_PROTOCOL_VERSION`'s v17 note, which tells
/// the operator to stop the process manually).
async fn drain_stale_agentd(socket_path: &Path) -> Result<(), String> {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    match establish_for_drain(stream).await {
        Ok((hub, conn_task)) => {
            // Bounded like every establish leg: an incompatible daemon that
            // accepts the connection but never answers must not pin the
            // recovery path.
            let _ = tokio::time::timeout(establish_timeout(), hub.drain()).await;
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("drain connection to the incompatible agentd failed: {error}");
        }
    }
    if wait_until_refusing(socket_path).await {
        Ok(())
    } else {
        Err(
            "horizon-agentd kept accepting connections after the drain call; \
             stop it manually"
                .to_string(),
        )
    }
}

/// A minimal establish for the drain path: connect + base handover only —
/// no `hello`, since the whole point is that `hello` already failed.
async fn establish_for_drain(
    stream: tokio::net::UnixStream,
) -> Result<(SessionHubClient<WireCodec>, ConnTask), String> {
    let deadline = tokio::time::Instant::now() + establish_timeout();
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
