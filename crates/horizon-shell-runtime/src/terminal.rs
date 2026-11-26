//! The `horizon-terminald` client connection: connect, negotiate, dispatch
//! terminal ops (`docs/terminald-split-design.md`).
//!
//! Stale-daemon recovery uses version-stable remoc calls and tries handover
//! before draining: a drain kills every PTY and the processes it hosts.
//! Schema negotiation does not cover transport framing or codec changes;
//! [`establish`] probes the connection after `hello`.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_terminal_core::wire::{
    TerminalAttachment, TerminalHub as _, TerminalHubClient, terminal_client_hello,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use horizon_wire::{DecodeSkipLog, HubError, WireCodec};
use remoc::rtc::Client as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

use super::common::{
    ConnTask, Connected, EstablishError, OP_TIMEOUT, RuntimeControl, SILENCE_MISMATCH_THRESHOLD,
    StreamEnd, Timeouts, connect_hub, establish_for_drain, wait_until_refusing, with_deadline,
};
use super::connection::connect_or_spawn_terminald_retrying;
use super::routing::{RouteKey, TerminalRoutes};

/// The daemon this module talks to, named in every classified error.
const DAEMON: &str = "horizon-terminald";

/// [`OP_TIMEOUT`]'s sibling for `create_terminal`, which is bounded
/// daemon-side by up to 3 × 10 s PTY spawn attempts (see
/// `TerminalHost::create`'s watchdog) — the client deadline must sit
/// above that whole budget or it would give up on a spawn the daemon is
/// still legitimately retrying.
const CREATE_TERMINAL_TIMEOUT: Duration = Duration::from_secs(45);

/// How long the skew check waits for the connection to report itself lost
/// before concluding that a failed post-`hello` probe was about the *data*
/// rather than the link. Short: `closed()` resolves as soon as the mux task
/// notices, and a live connection never resolves it at all.
const SKEW_LIVENESS_WINDOW: Duration = Duration::from_millis(200);

/// The action the user is told to take when the skew insurance fires — the
/// only command that replaces a running `horizon-terminald`.
const SKEW_REMEDY: &str = "rebuild (`cargo build --workspace`) and run `Reload Terminal Runtime`";

/// How long [`Op::HandOver`] waits for the daemon: each of its sessions
/// is bounded daemon-side, and a handover that worked ends the call by
/// closing the connection.
const HAND_OVER_TIMEOUT: Duration = Duration::from_secs(30);

/// One typed request from the sync world to the terminal runtime.
pub(super) enum Op {
    CreateTerminal {
        route: RouteKey<Uuid>,
        spec: Box<TerminalSpawnSpec>,
        commands: UnboundedReceiver<TerminalCommand>,
    },
    AttachTerminal {
        route: RouteKey<Uuid>,
        commands: UnboundedReceiver<TerminalCommand>,
        /// `true` exactly when the daemon reported a successful attach.
        reply: crossbeam_channel::Sender<bool>,
    },
    TerminalList {
        reply: crossbeam_channel::Sender<Result<Vec<TerminalSummary>, String>>,
    },
    Drain,
    HandOver {
        reply: crossbeam_channel::Sender<HandOver>,
    },
}

/// What asking the daemon to hand its terminals to its next image came to
/// (`docs/terminald-handover-design.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandOver {
    /// The image was replaced: the connection closed with the call.
    Done,
    /// The terminals went on in this daemon, for this reason.
    Refused(String),
    /// The daemon does not hand over (a build before it): it can only be
    /// drained.
    Unable,
}

pub(super) fn spawn(
    socket_path: PathBuf,
    control_socket: PathBuf,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<TerminalRoutes>,
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
                    "could not start the horizon-terminald client runtime: {error}"
                ));
                control.mark_stopped();
                return;
            }
        };
        runtime.block_on(async {
            let mut recovery = Recovery::None;
            let mut retry_delay = Duration::from_millis(50);
            let mut consecutive_silences: u32 = 0;
            loop {
                let stream = tokio::select! {
                    result = connect_or_spawn_terminald_retrying(
                        &socket_path,
                        &control_socket,
                    ) => match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("horizon-terminald initial connection failed: {error}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    },
                    _ = control.cancelled() => break,
                };

                match run_stream(stream, &mut ops, routes.clone(), control.clone()).await {
                    StreamEnd::PreHelloTransport { message } => {
                        consecutive_silences = 0;
                        eprintln!("horizon-terminald hello transport failed, retrying: {message}");
                        tokio::select! {
                            _ = tokio::time::sleep(retry_delay) => {}
                            _ = control.cancelled() => {
                                routes.connection_failed("terminald runtime stopped".to_string());
                                break;
                            }
                        }
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                        continue;
                    }
                    StreamEnd::Silence { message } => {
                        consecutive_silences += 1;
                        if consecutive_silences < SILENCE_MISMATCH_THRESHOLD {
                            eprintln!(
                                "horizon-terminald did not answer within the establish deadline \
                                 ({consecutive_silences}/{SILENCE_MISMATCH_THRESHOLD} before \
                                 mismatch recovery): {message}"
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(retry_delay) => {}
                                _ = control.cancelled() => {
                                    routes.connection_failed(
                                        "terminald runtime stopped".to_string(),
                                    );
                                    break;
                                }
                            }
                            retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                            continue;
                        }
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_stale_terminald(
                            &message,
                            &mut recovery,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::GenerationMismatch { message }
                    | StreamEnd::VersionRejected { message } => {
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_stale_terminald(
                            &message,
                            &mut recovery,
                            &socket_path,
                            &routes,
                            &control,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) => {
                        eprintln!("horizon-terminald connection stopped: {error}");
                        routes.connection_failed(error);
                        break;
                    }
                    StreamEnd::Cancelled => {
                        routes.connection_failed("terminald runtime stopped".to_string());
                        break;
                    }
                    StreamEnd::Dropped => break,
                }
            }
        });
        control.mark_stopped();
    });
}

/// How far [`recover_stale_terminald`] has gone, once per runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Recovery {
    None,
    /// Asked to hand over; the reconnect still could not talk to it.
    HandedOver,
    Drained,
}

/// The once-per-runtime recovery for a terminald this build cannot talk to,
/// over the version-stable rtc surface (no `hello` first, since the whole
/// point is that `hello` already failed). First it is asked to hand its
/// terminals to its next image, which is the build on disk: the caller's
/// reconnect then reaches that, with the terminals in it. A daemon that
/// cannot hand over closes the connection all the same and still cannot
/// be talked to after it; that one is drained, which kills the PTYs and is
/// acceptable only because the alternative is a terminal daemon nobody can
/// reach at all, and the caller's next `connect_or_spawn_terminald_retrying`
/// starts a fresh binary. Neither step is taken twice, so a stale binary
/// can never loop on them.
async fn recover_stale_terminald(
    message: &str,
    recovery: &mut Recovery,
    socket_path: &Path,
    routes: &Arc<TerminalRoutes>,
    control: &Arc<RuntimeControl>,
) -> ControlFlow<()> {
    if *recovery == Recovery::None {
        *recovery = Recovery::HandedOver;
        eprintln!("{message}; asking horizon-terminald to hand over to the build on disk");
        tokio::select! {
            () = hand_over_stale_terminald(socket_path, control.timeouts()) => {}
            _ = control.cancelled() => {
                routes.connection_failed("terminald runtime stopped".to_string());
                return ControlFlow::Break(());
            }
        }
        return ControlFlow::Continue(());
    }
    if *recovery == Recovery::Drained {
        let error = format!(
            "{message} -- automatic handover and drain-and-restart were already attempted \
             once; {SKEW_REMEDY}"
        );
        eprintln!("horizon-terminald connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    *recovery = Recovery::Drained;
    eprintln!("{message}; draining and restarting horizon-terminald");
    let drained = tokio::select! {
        drained = drain_stale_terminald(socket_path, control.timeouts()) => drained,
        _ = control.cancelled() => {
            routes.connection_failed("terminald runtime stopped".to_string());
            return ControlFlow::Break(());
        }
    };
    if let Err(error) = drained {
        let error = format!("{message} -- and the automatic drain failed: {error}");
        eprintln!("horizon-terminald connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
pub(super) fn spawn_test_stream<S>(
    stream: S,
    mut ops: UnboundedReceiver<Op>,
    routes: Arc<TerminalRoutes>,
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
            // Recovery needs a real socket to drain and a daemon to
            // respawn; a test stream has neither, so these surface as
            // terminal failures instead.
            StreamEnd::GenerationMismatch { message } | StreamEnd::VersionRejected { message } => {
                routes.connection_failed(message)
            }
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
    hub: TerminalHubClient<WireCodec>,
    routes: Arc<TerminalRoutes>,
    /// What the daemon said of [`HandOver`] in its `hello`.
    hands_over: bool,
}

async fn run_stream<S>(
    stream: S,
    ops: &mut UnboundedReceiver<Op>,
    routes: Arc<TerminalRoutes>,
    control: Arc<RuntimeControl>,
) -> StreamEnd
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let established = tokio::select! {
        result = establish(stream, control.timeouts().establish) => result,
        _ = control.cancelled() => return StreamEnd::Cancelled,
    };
    let (hub, conn_task, hands_over) = match established {
        Ok(established) => established,
        Err(error) => return error.into(),
    };
    control.mark_established();

    let live = Live {
        hub: hub.clone(),
        routes: routes.clone(),
        hands_over,
    };

    let mut closed = hub.closed();
    let end = loop {
        tokio::select! {
            _ = control.cancelled() => break StreamEnd::Cancelled,
            _ = &mut closed => {
                break StreamEnd::EstablishedFailure(
                    "established terminald disconnected".to_string(),
                );
            }
            op = ops.recv() => {
                let Some(op) = op else {
                    break StreamEnd::Dropped;
                };
                handle_op(op, &live, control.timeouts().establish);
            }
        }
    };
    conn_task.abort();
    end
}

/// The client, its connection, and whether the daemon hands over.
type EstablishedParts = (TerminalHubClient<WireCodec>, ConnTask, bool);

/// Runs the shared connect prelude ([`connect_hub`]), then this hub's own
/// Runs `hello` under the establishment deadline, then probes with `list_terminals`.
/// A failed probe on a live connection signals skew below the wire schema;
/// the error names the peer build and suggests `Reload Terminal Runtime`.
/// The probe cannot detect skew confined to other message types. Live attachment
/// channels skip individual decode failures and rate-limit their logs.
async fn establish<S>(stream: S, timeout: Duration) -> Result<EstablishedParts, EstablishError>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let Connected {
        hub,
        conn_task,
        deadline,
        timeout,
    } = connect_hub::<_, TerminalHubClient<WireCodec>>(stream, DAEMON, timeout).await?;

    let client_hello = terminal_client_hello(concat!("horizon/", env!("CARGO_PKG_VERSION")));
    let hello = match tokio::time::timeout_at(deadline, hub.hello(client_hello)).await {
        Ok(Ok(hello)) => hello,
        Ok(Err(error @ HubError::IncompatibleVersion { .. })) => {
            conn_task.abort();
            return Err(EstablishError::Rejected(format!(
                "terminald rejected the handshake: {error}"
            )));
        }
        Ok(Err(error @ HubError::Call(_))) => {
            conn_task.abort();
            return Err(EstablishError::Transient(format!(
                "the connection dropped during hello: {error}"
            )));
        }
        Ok(Err(error)) => {
            conn_task.abort();
            return Err(EstablishError::Fatal(format!(
                "terminald answered hello with an unexpected error: {error}"
            )));
        }
        Err(_elapsed) => {
            conn_task.abort();
            return Err(EstablishError::Silence(format!(
                "terminald did not answer hello within {timeout:?}"
            )));
        }
    };

    if let Err(probe_error) =
        with_deadline(OP_TIMEOUT, "list_terminals", hub.list_terminals()).await
    {
        // Is the link gone, or is it the data? A connection that has
        // already dropped is an ordinary transient (the daemon died right
        // after `hello`); one that is still up answered our probe with
        // something we could not use, which is the skew signal.
        let link_lost = tokio::time::timeout(SKEW_LIVENESS_WINDOW, hub.closed())
            .await
            .is_ok();
        conn_task.abort();
        if link_lost {
            return Err(EstablishError::Transient(format!(
                "terminald dropped the connection right after hello: {probe_error}"
            )));
        }
        return Err(EstablishError::Fatal(format!(
            "horizon-terminald ({}) answered hello but not the first call after it \
             ({probe_error}) -- this build cannot talk to that daemon below the negotiated \
             protocol version; refusing to continue. Fix: {SKEW_REMEDY}",
            hello.binary_id
        )));
    }

    Ok((hub, conn_task, hello.hands_over))
}

/// Dispatches one op. Every rtc call runs on its own task (the calls are
/// independent and a slow one — a PTY spawn — must not stall command
/// forwarding for other sessions), holding clones of the hub client and
/// routes.
fn handle_op(op: Op, live: &Live, establish: Duration) {
    match op {
        Op::CreateTerminal {
            route,
            spec,
            commands,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match with_deadline(
                    CREATE_TERMINAL_TIMEOUT,
                    "create_terminal",
                    hub.create_terminal(route.session_id(), *spec),
                )
                .await
                {
                    Ok(attachment) => {
                        run_terminal_attachment(routes, route, attachment, commands).await
                    }
                    // What the JSONL wire delivered as a
                    // `TerminalUpdate::Error` on the update stream.
                    Err(error) => routes.terminal_failed(route, error),
                }
            });
        }
        Op::AttachTerminal {
            route,
            commands,
            reply,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match with_deadline(
                    OP_TIMEOUT,
                    "attach_terminal",
                    hub.attach_terminal(route.session_id()),
                )
                .await
                {
                    Ok(attachment) => {
                        let _ = reply.send(true);
                        run_terminal_attachment(routes, route, attachment, commands).await;
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
                let result = with_deadline(OP_TIMEOUT, "terminal list", hub.list_terminals()).await;
                let _ = reply.send(result);
            });
        }
        Op::Drain => {
            let hub = live.hub.clone();
            tokio::spawn(async move {
                // The daemon exits inside this call, so the reply usually
                // never arrives; completion is observed by the caller as
                // the socket refusing connections (`wait_for_drain`).
                let _ = tokio::time::timeout(establish, hub.drain()).await;
            });
        }
        Op::HandOver { reply } => {
            if !live.hands_over {
                let _ = reply.send(HandOver::Unable);
                return;
            }
            let hub = live.hub.clone();
            tokio::spawn(async move {
                let handed = match tokio::time::timeout(HAND_OVER_TIMEOUT, hub.hand_over()).await {
                    Ok(Ok(refused)) => HandOver::Refused(refused.reason),
                    // The image is replaced inside the call, and the
                    // connection closes with it.
                    Ok(Err(_)) => HandOver::Done,
                    Err(_) => HandOver::Refused(format!(
                        "horizon-terminald did not hand over within {HAND_OVER_TIMEOUT:?}"
                    )),
                };
                let _ = reply.send(handed);
            });
        }
    }
}

/// One live terminal attachment: forwards handle commands to the daemon,
/// and its events to the pane, until either side goes away.
async fn run_terminal_attachment(
    routes: Arc<TerminalRoutes>,
    route: RouteKey<Uuid>,
    attachment: TerminalAttachment,
    mut commands: UnboundedReceiver<TerminalCommand>,
) {
    let TerminalAttachment {
        mut events,
        commands: remote_commands,
    } = attachment;
    let mut event_skips = DecodeSkipLog::new("terminal events");
    let mut command_skips = DecodeSkipLog::new("terminal commands");

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if let Err(err) = remote_commands.send(command).await {
                        // rch latches remote-send errors on the sender
                        // (one failure means every later send fails too),
                        // so any send error ends the attachment rather
                        // than skip-looping. Oversized commands are
                        // enforced daemon-side as per-item *receive*
                        // skips, so they never surface here.
                        command_skips.note(&err);
                        break;
                    }
                }
                // The pane's handle (and its bridge thread) are gone.
                None => break,
            },
            event = events.recv() => match event {
                Ok(Some(update)) => {
                    let exited = matches!(update, TerminalUpdate::Exited);
                    routes.route_terminal_update(route, update);
                    if exited {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) if err.is_disconnected() => break,
                // Adoption condition 2: one undecodable event is skipped;
                // the channel survives.
                Err(err) => event_skips.note(&err),
            },
        }
    }
}

/// Gracefully stops a terminald this build cannot negotiate with: `hello`
/// and `drain` are the version-stable hub surface, so the drain travels as
/// an ordinary rtc call on a fresh connection — no `hello` first, since the
/// whole point is that `hello` already failed.
/// Asks a terminald this build cannot negotiate with to hand over, on a
/// fresh connection with no `hello`, as [`drain_stale_terminald`] drains
/// one. Its listening socket goes to the next image, so nothing tells a
/// handover from a daemon that only dropped the call: the reconnect after
/// it does.
async fn hand_over_stale_terminald(socket_path: &Path, timeouts: Timeouts) {
    let Ok(stream) = tokio::net::UnixStream::connect(socket_path).await else {
        return;
    };
    match establish_for_drain::<TerminalHubClient<WireCodec>>(stream, timeouts.establish).await {
        Ok((hub, conn_task)) => {
            if let Ok(Ok(refused)) = tokio::time::timeout(HAND_OVER_TIMEOUT, hub.hand_over()).await
            {
                eprintln!("horizon-terminald kept its terminals: {}", refused.reason);
            }
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("handover connection to the incompatible terminald failed: {error}");
        }
    }
}

async fn drain_stale_terminald(socket_path: &Path, timeouts: Timeouts) -> Result<(), String> {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    match establish_for_drain::<TerminalHubClient<WireCodec>>(stream, timeouts.establish).await {
        Ok((hub, conn_task)) => {
            let _ = tokio::time::timeout(timeouts.establish, hub.drain()).await;
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("drain connection to the incompatible terminald failed: {error}");
        }
    }
    if wait_until_refusing(socket_path, timeouts.drain_exit).await {
        Ok(())
    } else {
        Err(
            "horizon-terminald kept accepting connections after the drain call; \
             stop it manually"
                .to_string(),
        )
    }
}
