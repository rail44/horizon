use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use horizon_agent::contract::{self, Command};
use horizon_agent::wire::{self, HostToolResponse};
use horizon_session_protocol::{
    legacy, CappedReceiver, ClientHello, DecodeSkipLog, HubError, HubHello, SessionHub as _,
    SessionHubClient, TerminalAttachment, WireCodec, CONTROL_MAX_ITEM_BYTES, RTC_MAX_REPLY_BYTES,
    RTC_MAX_REQUEST_BYTES, SESSION_PROTOCOL_VERSION, TOOL_IO_MAX_ITEM_BYTES,
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
/// daemon blocks in `read_line` waiting for a newline our chmux hello
/// never contains (measured — the v9 pre-hello loop is *silent* against
/// chmux bytes), so it presents as this timeout — never chmux's own raw
/// 60 s `ChMux(Timeout)`.
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Test-only override for [`ESTABLISH_TIMEOUT`]
/// (`HORIZON_TEST_ESTABLISH_TIMEOUT_MS`): the silence-escalation tests
/// below would otherwise take `SILENCE_MISMATCH_THRESHOLD × 5 s` of real
/// wall clock per stale daemon. Never set in production.
fn establish_timeout() -> Duration {
    std::env::var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(ESTABLISH_TIMEOUT)
}

/// How many *consecutive* silent establish timeouts equal "a JSONL
/// generation daemon holds the socket" (`docs/remoc-adoption-design.md`
/// §6's bounded-timeout detection). One timeout is not evidence — a
/// healthy daemon can be transiently unresponsive (its one-at-a-time
/// accept loop busy, host under load) and the once-per-runtime recovery
/// budget must not be burned on that — but a *v9 daemon is silent every
/// time* (measured: its pre-hello `read_line` never completes on chmux
/// bytes), so persistence is the signal.
const SILENCE_MISMATCH_THRESHOLD: u32 = 3;

/// Deadline for one established-phase rtc call (`list_terminals`,
/// `list_agents`, `attach_terminal`, `attach_agent`, `new_agent`). Not
/// tight on purpose: `list_agents`/`new_agent`/`attach_agent` legitimately
/// block on the daemon's resume-readiness gate (a large event log takes
/// real seconds to resume), so a short deadline would misreport a healthy
/// startup as a failure. A timeout fails only that op — the runtime and
/// connection survive.
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// [`OP_TIMEOUT`]'s sibling for `create_terminal`, which is bounded
/// daemon-side by up to 3 × 10 s PTY spawn attempts (see
/// `TerminalHost::create`'s watchdog) — the client deadline must sit
/// above that whole budget or it would give up on a spawn the daemon is
/// still legitimately retrying.
const CREATE_TERMINAL_TIMEOUT: Duration = Duration::from_secs(45);

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
            // The transient-retry backoff (the JSONL era's
            // `hello_retry_delay`, restored) and the consecutive-silence
            // counter behind `SILENCE_MISMATCH_THRESHOLD`.
            let mut retry_delay = Duration::from_millis(50);
            let mut consecutive_silences: u32 = 0;
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
                    StreamEnd::PreHelloTransport { message } => {
                        // Transient: retry with backoff, never consuming the
                        // recovery budget. A differently-shaped failure also
                        // breaks any "persistent silence" pattern.
                        consecutive_silences = 0;
                        eprintln!("horizon-sessiond hello transport failed, retrying: {message}");
                        tokio::select! {
                            _ = tokio::time::sleep(retry_delay) => {}
                            _ = control.cancelled() => {
                                routes.connection_failed("sessiond runtime stopped".to_string());
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
                                "horizon-sessiond did not answer within the establish deadline \
                                 ({consecutive_silences}/{SILENCE_MISMATCH_THRESHOLD} before \
                                 mismatch recovery): {message}"
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(retry_delay) => {}
                                _ = control.cancelled() => {
                                    routes.connection_failed(
                                        "sessiond runtime stopped".to_string(),
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

/// The once-per-runtime JSONL-generation recovery
/// (`docs/remoc-adoption-design.md` §6, extending PR #18's decisions):
/// drain the stale daemon at *its own* envelope version via the
/// quarantined legacy encoder, then let the caller's next
/// `connect_or_spawn_retrying` start a fresh binary. `Break` means the
/// runtime must stop (budget already spent, drain failed, or cancelled)
/// -- `connection_failed` has already been fanned out then; `Continue`
/// means recovery succeeded and the caller should reconnect.
async fn recover_generation_mismatch(
    message: &str,
    mismatch_recovery_attempted: &mut bool,
    socket_path: &Path,
    routes: &Arc<Routes>,
    control: &Arc<RuntimeControl>,
) -> ControlFlow<()> {
    if *mismatch_recovery_attempted {
        // If the respawned daemon still can't speak remoc (a stale
        // horizon-sessiond binary -- `cargo run` rebuilds only the horizon
        // binary), restarting it again would loop forever, so give up
        // loudly instead.
        let error = format!(
            "{message} -- automatic drain-and-restart was already attempted \
             once; rebuild horizon-sessiond (`cargo build --workspace`) and \
             run `Reload Session Runtime`"
        );
        eprintln!("horizon-sessiond connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    *mismatch_recovery_attempted = true;
    eprintln!(
        "a horizon-sessiond that does not speak the v{SESSION_PROTOCOL_VERSION} \
         remoc wire detected ({message}); draining and restarting it"
    );
    let drained = tokio::select! {
        drained = drain_stale_sessiond(socket_path) => drained,
        _ = control.cancelled() => {
            routes.connection_failed("sessiond runtime stopped".to_string());
            return ControlFlow::Break(());
        }
    };
    if let Err(error) = drained {
        let error = format!("{message} -- and the automatic drain failed: {error}");
        eprintln!("horizon-sessiond connection stopped: {error}");
        routes.connection_failed(error);
        return ControlFlow::Break(());
    }
    ControlFlow::Continue(())
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

enum StreamEnd {
    /// A transient pre-hello failure — connection drop, IO error, base
    /// channel EOF, or the `hello` call's own transport failure
    /// (`HubError::Call`). Retried with backoff, exactly like the JSONL
    /// era's `PreHelloTransport`; **never** consumes the once-per-runtime
    /// mismatch-recovery budget (a daemon crash or a busy host must not
    /// eat the one automatic drain-and-restart this runtime gets).
    PreHelloTransport {
        message: String,
    },
    /// The peer stayed silent for the whole establish deadline. One
    /// occurrence is treated like a transient (retried); only
    /// [`SILENCE_MISMATCH_THRESHOLD`] *consecutive* silences escalate to
    /// the generation-mismatch recovery, because persistent silence is
    /// exactly how a real v9 JSONL daemon presents (measured: its
    /// pre-hello `read_line` blocks forever on chmux bytes) while a
    /// healthy remoc daemon answers in milliseconds.
    Silence {
        message: String,
    },
    /// Positive garbage evidence: the peer *sent bytes that are not
    /// chmux* (a length-prefix/framing violation — e.g. JSONL text reads
    /// as an absurd frame length — or a chmux protocol error). No healthy
    /// remoc daemon can produce this, so it consumes the recovery budget
    /// immediately. A daemon that died mid-handshake does **not** land
    /// here (that is [`Self::PreHelloTransport`]).
    GenerationMismatch {
        message: String,
    },
    /// The daemon speaks remoc and answered `hello` with an explicit
    /// version-range rejection. Recoverable via an rtc `drain`; consumes
    /// the recovery budget.
    VersionRejected {
        message: String,
    },
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
        Err(EstablishError::Transient(message)) => return StreamEnd::PreHelloTransport { message },
        Err(EstablishError::Silence(message)) => return StreamEnd::Silence { message },
        Err(EstablishError::Garbage(message)) => return StreamEnd::GenerationMismatch { message },
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
    /// See [`StreamEnd::PreHelloTransport`].
    Transient(String),
    /// See [`StreamEnd::Silence`].
    Silence(String),
    /// See [`StreamEnd::GenerationMismatch`] — received non-chmux bytes.
    Garbage(String),
    /// The daemon's hub rejected our version range.
    Rejected(String),
    Fatal(String),
}

/// Classifies a failed `Connect::io`: framing/protocol violations are
/// positive "the peer is not speaking chmux" evidence (measured: a JSONL
/// line read as a chmux length prefix fails instantly with a
/// `LengthDelimitedCodecError` under `ErrorKind::InvalidData`; a decodable
/// frame with an invalid multiplex message is `ChMuxError::Protocol`);
/// everything else — closes, resets, plain IO errors — is transient.
fn classify_connect_error(
    error: &remoc::ConnectError<std::io::Error, std::io::Error>,
) -> EstablishError {
    use remoc::chmux::ChMuxError;
    let garbage = match error {
        remoc::ConnectError::ChMux(ChMuxError::Protocol(_)) => true,
        remoc::ConnectError::ChMux(ChMuxError::StreamError(io_error)) => {
            io_error.kind() == std::io::ErrorKind::InvalidData
        }
        _ => false,
    };
    if garbage {
        EstablishError::Garbage(format!(
            "sessiond sent bytes that are not remoc/chmux (likely a stale pre-v10 JSONL \
             daemon): {error}"
        ))
    } else {
        EstablishError::Transient(format!("remoc connect to sessiond failed: {error}"))
    }
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
    let timeout = establish_timeout();
    let deadline = tokio::time::Instant::now() + timeout;
    let (read_half, write_half) = tokio::io::split(stream);

    let connect = remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
        remoc::Cfg::default(),
        read_half,
        write_half,
    );
    let (conn, _base_tx, mut base_rx) = match tokio::time::timeout_at(deadline, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => return Err(classify_connect_error(&error)),
        Err(_elapsed) => {
            return Err(EstablishError::Silence(format!(
                "sessiond sent no remoc handshake within {timeout:?}"
            )))
        }
    };
    let conn_task = tokio::spawn(conn);

    let mut hub = match tokio::time::timeout_at(deadline, base_rx.recv()).await {
        Ok(Ok(Some(hub))) => hub,
        // Base-channel EOF/errors: the chmux handshake *did* complete, so
        // the peer speaks remoc — a drop here is a dying daemon, not a
        // generation signal. Transient.
        Ok(Ok(None)) | Ok(Err(_)) => {
            conn_task.abort();
            return Err(EstablishError::Transient(
                "sessiond closed the connection before handing over its hub client".to_string(),
            ));
        }
        Err(_elapsed) => {
            conn_task.abort();
            return Err(EstablishError::Silence(format!(
                "sessiond handed over no hub client within {timeout:?}"
            )));
        }
    };

    // The reply cap travels with each request (the macro caps the
    // per-call reply channel from this value), so setting it here is the
    // effective knob for what this client will accept per reply. The
    // request cap, by contrast, is enforced daemon-side from the value
    // the daemon set before transporting this client — the local set
    // below only re-documents the intended bound (a transported sender's
    // local cap is not re-checked).
    hub.set_max_request_size(RTC_MAX_REQUEST_BYTES);
    hub.set_max_reply_size(RTC_MAX_REPLY_BYTES);

    let client_hello = ClientHello::new(concat!("horizon/", env!("CARGO_PKG_VERSION")));
    match tokio::time::timeout_at(deadline, hub.hello(client_hello)).await {
        Ok(Ok(hello)) => Ok((hub, hello, conn_task)),
        Ok(Err(error @ HubError::IncompatibleVersion { .. })) => {
            conn_task.abort();
            Err(EstablishError::Rejected(format!(
                "sessiond rejected the handshake: {error}"
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
                "sessiond answered hello with an unexpected error: {error}"
            )))
        }
        Err(_elapsed) => {
            conn_task.abort();
            Err(EstablishError::Silence(format!(
                "sessiond did not answer hello within {timeout:?}"
            )))
        }
    }
}

fn spawn_host_tool_pump(
    mut host_tools: CappedReceiver<wire::HostToolRequest, TOOL_IO_MAX_ITEM_BYTES>,
    routes: Arc<Routes>,
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
    }
}

/// Bounds one established-phase rtc call. A deadline expiry fails only
/// that call (the reply channel gets an error, or the routes get a
/// per-session failure) — the runtime and the connection stay up, because
/// a wedged single call must not take down every other live attachment.
async fn with_deadline<T>(
    deadline: Duration,
    what: &str,
    call: impl std::future::Future<Output = Result<T, HubError>>,
) -> Result<T, String> {
    match tokio::time::timeout(deadline, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{what} failed: {error}")),
        Err(_elapsed) => Err(format!("{what} did not answer within {deadline:?}")),
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
    let mut update_skips = DecodeSkipLog::new("terminal updates");
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
                Err(err) => update_skips.note(&err),
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
    let mut event_skips = DecodeSkipLog::new("agent events");
    let mut command_skips = DecodeSkipLog::new("agent commands");
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if let Err(err) = remote_commands.send(command).await {
                        // See the terminal runner: send errors latch.
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
    for version in (legacy::OLDEST_DRAINABLE_VERSION..=legacy::NEWEST_JSONL_VERSION).rev() {
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
            // Bounded like every establish leg: an incompatible daemon that
            // accepts the connection but never answers must not pin the
            // recovery path.
            let _ = tokio::time::timeout(establish_timeout(), hub.drain()).await;
            conn_task.abort();
        }
        Err(error) => {
            eprintln!("drain connection to the incompatible sessiond failed: {error}");
        }
    }
    if wait_until_refusing(socket_path).await {
        Ok(())
    } else {
        Err(
            "horizon-sessiond kept accepting connections after the drain call; \
             stop it manually"
                .to_string(),
        )
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
