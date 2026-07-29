//! The `horizon-terminald` client connection: connect, negotiate, dispatch
//! terminal ops (`docs/terminald-split-design.md`).
//!
//! Structurally the twin of [`super::connection`], with two deliberate
//! differences that follow from what this daemon *is*:
//!
//! 1. **No JSONL generation to recover from.** `horizon-terminald` was born
//!    at wire v17, so there is no pre-remoc encoder to probe: a stale
//!    terminald is always a remoc peer, and the one recovery path is the
//!    version-stable rtc `drain`.
//! 2. **Below-schema skew insurance** (design decision 6, the mechanized
//!    tmux 3.6 lesson). This daemon is deliberately long-lived, so the
//!    binary it is running may predate the client by many rebuilds — and
//!    version negotiation only covers the *schema*, never the transport
//!    beneath it (chmux framing, the Postbag codec). After `hello` the
//!    client therefore issues one cheap probe call and, if it fails while
//!    the connection is still up, refuses cleanly with a message naming the
//!    peer's `binary_id` and `Reload Terminal Runtime` instead of carrying
//!    on with a peer it cannot actually talk to. See [`establish`].

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_session_protocol::{
    ClientHello, DecodeSkipLog, HubError, TerminalAttachment, TerminalHub as _, TerminalHubClient,
    TerminalHubHello, WireCodec, RTC_MAX_REPLY_BYTES, RTC_MAX_REQUEST_BYTES,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};
use remoc::rtc::Client as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::common::{
    classify_connect_error, establish_timeout, wait_until_refusing, with_deadline, EstablishError,
    RuntimeControl, StreamEnd, OP_TIMEOUT, SILENCE_MISMATCH_THRESHOLD,
};
use super::routing::TerminalRoutes;

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
const SKEW_REMEDY: &str = "rebuild (`cargo build --workspace`) and run `Reload Terminal Runtime` \
                           (this terminates every terminal session)";

/// One typed request from the sync world to the terminal runtime.
pub(super) enum Op {
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
    Drain,
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
            let mut mismatch_recovery_attempted = false;
            let mut retry_delay = Duration::from_millis(50);
            let mut consecutive_silences: u32 = 0;
            loop {
                let stream = tokio::select! {
                    result = horizon_agent::client::connect_or_spawn_terminald_retrying(
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
                    // Garbage and an explicit range rejection get the same
                    // recovery here: unlike sessiond there is no older
                    // envelope generation to probe downward through, so the
                    // one honest move for either is the version-stable rtc
                    // drain followed by a respawn.
                    StreamEnd::GenerationMismatch { message }
                    | StreamEnd::VersionRejected { message } => {
                        consecutive_silences = 0;
                        if let ControlFlow::Break(()) = recover_stale_terminald(
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

/// The once-per-runtime recovery for a terminald this build cannot talk to:
/// drain it over the version-stable rtc surface, then let the caller's next
/// `connect_or_spawn_terminald_retrying` start a fresh binary. Draining is
/// *destructive* (it kills the PTYs), which is acceptable only because the
/// alternative is a terminal daemon nobody can reach at all — and it is
/// budgeted at once per runtime so a stale binary can never loop on it.
async fn recover_stale_terminald(
    message: &str,
    mismatch_recovery_attempted: &mut bool,
    socket_path: &Path,
    routes: &Arc<TerminalRoutes>,
    control: &Arc<RuntimeControl>,
) -> ControlFlow<()> {
    if *mismatch_recovery_attempted {
        let error = format!(
            "{message} -- automatic drain-and-restart was already attempted once; {SKEW_REMEDY}"
        );
        eprintln!("horizon-terminald connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    *mismatch_recovery_attempted = true;
    eprintln!("{message}; draining and restarting horizon-terminald");
    let drained = tokio::select! {
        drained = drain_stale_terminald(socket_path) => drained,
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
        result = establish(stream) => result,
        _ = control.cancelled() => return StreamEnd::Cancelled,
    };
    let (hub, hello, conn_task) = match established {
        Ok(established) => established,
        Err(EstablishError::Transient(message)) => return StreamEnd::PreHelloTransport { message },
        Err(EstablishError::Silence(message)) => return StreamEnd::Silence { message },
        Err(EstablishError::Garbage(message)) => return StreamEnd::GenerationMismatch { message },
        Err(EstablishError::Rejected(message)) => return StreamEnd::VersionRejected { message },
        Err(EstablishError::Fatal(message)) => return StreamEnd::Fatal(message),
    };
    control.mark_established();
    control.set_negotiated(hello.negotiated);

    let live = Live {
        hub: hub.clone(),
        routes: routes.clone(),
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
                handle_op(op, &live);
            }
        }
    };
    conn_task.abort();
    end
}

type EstablishedParts = (
    TerminalHubClient<WireCodec>,
    TerminalHubHello,
    JoinHandle<Result<(), remoc::chmux::ChMuxError<std::io::Error, std::io::Error>>>,
);

/// Runs the remoc connect + base handover + `hello` + the decision-6 skew
/// probe, each leg bounded by one shared establish deadline.
///
/// **The probe and what it does / does not catch.** After a successful
/// `hello` the client calls `list_terminals` once — the cheapest reply-
/// bearing method on the hub — and treats a failure *while the connection
/// is still up* as evidence that this peer's serialization does not match
/// ours below the negotiated schema. That is the honest reading: the only
/// errors `list_terminals` can return are transport-shaped, and a peer that
/// answered `hello` has already proved it is alive and speaking chmux.
///
/// It **catches**: a peer whose reply encoding for a small structured value
/// diverges from ours (a codec change, a chmux/rch framing change under a
/// terminald binary older than the client), and any post-`hello` refusal a
/// stale binary produces. It **does not catch**: skew confined to types the
/// probe never exercises (frames, commands, the attachment channels
/// themselves), nor a change that leaves both `hello` and an empty
/// `list_terminals` reply byte-compatible. Per-item decode failures on the
/// live attachment channels stay *tolerant* (skipped and rate-limit
/// logged, adoption condition 2) rather than escalating here: one poisoned
/// frame must not kill every running shell, which is precisely the outcome
/// this whole split exists to prevent.
async fn establish<S>(stream: S) -> Result<EstablishedParts, EstablishError>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
{
    let timeout = establish_timeout();
    let deadline = tokio::time::Instant::now() + timeout;
    let (read_half, write_half) = tokio::io::split(stream);

    let connect = remoc::Connect::io::<_, _, (), TerminalHubClient<WireCodec>, WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, _base_tx, mut base_rx) = match tokio::time::timeout_at(deadline, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => return Err(classify_connect_error(DAEMON, &error)),
        Err(_elapsed) => {
            return Err(EstablishError::Silence(format!(
                "terminald sent no remoc handshake within {timeout:?}"
            )))
        }
    };
    let conn_task = tokio::spawn(conn);

    let mut hub = match tokio::time::timeout_at(deadline, base_rx.recv()).await {
        Ok(Ok(Some(hub))) => hub,
        Ok(Ok(None)) | Ok(Err(_)) => {
            conn_task.abort();
            return Err(EstablishError::Transient(
                "terminald closed the connection before handing over its hub client".to_string(),
            ));
        }
        Err(_elapsed) => {
            conn_task.abort();
            return Err(EstablishError::Silence(format!(
                "terminald handed over no hub client within {timeout:?}"
            )));
        }
    };

    hub.set_max_request_size(RTC_MAX_REQUEST_BYTES);
    hub.set_max_reply_size(RTC_MAX_REPLY_BYTES);

    let client_hello = ClientHello::new(concat!("horizon/", env!("CARGO_PKG_VERSION")));
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

    Ok((hub, hello, conn_task))
}

/// Dispatches one op. Every rtc call runs on its own task (the calls are
/// independent and a slow one — a PTY spawn — must not stall command
/// forwarding for other sessions), holding clones of the hub client and
/// routes.
fn handle_op(op: Op, live: &Live) {
    match op {
        Op::CreateTerminal {
            session_id,
            spec,
            commands,
        } => {
            let hub = live.hub.clone();
            let routes = live.routes.clone();
            tokio::spawn(async move {
                match with_deadline(
                    CREATE_TERMINAL_TIMEOUT,
                    "create_terminal",
                    hub.create_terminal(session_id, *spec),
                )
                .await
                {
                    Ok(attachment) => {
                        run_terminal_attachment(routes, session_id, attachment, commands).await
                    }
                    // What the JSONL wire delivered as a
                    // `TerminalUpdate::Error` on the update stream.
                    Err(error) => routes.terminal_failed(session_id, error),
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
                match with_deadline(
                    OP_TIMEOUT,
                    "attach_terminal",
                    hub.attach_terminal(session_id),
                )
                .await
                {
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
                let _ = tokio::time::timeout(establish_timeout(), hub.drain()).await;
            });
        }
    }
}

/// One live terminal attachment: forwards handle commands to the daemon,
/// routes full frames (from the `watch<TerminalFrame>`, wire v11) and
/// non-frame events to the pane, until either side goes away.
async fn run_terminal_attachment(
    routes: Arc<TerminalRoutes>,
    session_id: Uuid,
    attachment: TerminalAttachment,
    mut commands: UnboundedReceiver<TerminalCommand>,
) {
    let TerminalAttachment {
        mut frames,
        mut events,
        commands: remote_commands,
    } = attachment;
    let mut frame_skips = DecodeSkipLog::new("terminal frames");
    let mut event_skips = DecodeSkipLog::new("terminal events");
    let mut command_skips = DecodeSkipLog::new("terminal commands");

    // `false` once the frame watch is closed (or its port setup failed): we
    // stop polling it, but keep servicing `events` so a clean shutdown's
    // `Exited` (which races the watch close) is never dropped — otherwise a
    // frames-close winning the select would strand a zombie pane.
    let mut frames_open = true;

    // Deliver the seed frame first: the watch receiver's initial value (the
    // daemon-retained latest frame on attach, or the empty create-time seed)
    // is read with `borrow`, not `changed` — `changed` only fires for
    // genuinely newer values, so without this an idle reattach would show a
    // blank grid forever. A non-final error (a skewed/undecodable seed value,
    // `is_final() == false`) is skipped, self-healing on the next frame; a
    // final error means the frame port is gone, so stop polling it.
    match frames.borrow_and_update() {
        Ok(seed) => routes.route_terminal_frame(session_id, seed.clone()),
        Err(err) if err.is_final() => frames_open = false,
        Err(err) => frame_skips.note(&err),
    }

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
            changed = frames.changed(), if frames_open => match changed {
                // A new frame is available. The watch keeps only the latest,
                // so a slow UI skips intermediate frames and converges on the
                // final value here (§5 Option A / spike §1c) — the client's
                // own row-comparison then invalidates just the changed rows.
                Ok(()) => match frames.borrow_and_update() {
                    Ok(frame) => routes.route_terminal_frame(session_id, frame.clone()),
                    // Non-final (a `Deserialize`/`MaxItemSizeExceeded` value
                    // the watch publishes but keeps the channel for): skip
                    // it and wait for the next frame, exactly like the events
                    // and seed paths (adoption condition 2, self-heal §5).
                    Err(err) if !err.is_final() => frame_skips.note(&err),
                    // Final: the frame port is gone. Stop polling frames but
                    // keep servicing events for a still-pending `Exited`.
                    Err(_) => frames_open = false,
                },
                // The watch (sender or connection) is gone — same handling:
                // stop polling frames, keep draining events.
                Err(_closed) => frames_open = false,
            },
            event = events.recv() => match event {
                Ok(Some(update)) => {
                    let exited = matches!(update, TerminalUpdate::Exited);
                    routes.route_terminal_update(session_id, update);
                    if exited {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) if err.is_final() => break,
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
async fn drain_stale_terminald(socket_path: &Path) -> Result<(), String> {
    let stream = match tokio::net::UnixStream::connect(socket_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    match establish_for_drain(stream).await {
        Ok((hub, conn_task)) => {
            let _ = tokio::time::timeout(establish_timeout(), hub.drain()).await;
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("drain connection to the incompatible terminald failed: {error}");
        }
    }
    if wait_until_refusing(socket_path).await {
        Ok(())
    } else {
        Err(
            "horizon-terminald kept accepting connections after the drain call; \
             stop it manually"
                .to_string(),
        )
    }
}

async fn establish_for_drain(
    stream: tokio::net::UnixStream,
) -> Result<
    (
        TerminalHubClient<WireCodec>,
        JoinHandle<Result<(), remoc::chmux::ChMuxError<std::io::Error, std::io::Error>>>,
    ),
    String,
> {
    let deadline = tokio::time::Instant::now() + establish_timeout();
    let (read_half, write_half) = stream.into_split();
    let connect = remoc::Connect::io::<_, _, (), TerminalHubClient<WireCodec>, WireCodec>(
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
