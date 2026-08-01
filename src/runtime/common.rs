//! What both daemon client runtimes share: the per-runtime control handle,
//! the remoc connect prelude every establishment opens with, the
//! establishment deadlines and their classification, the per-call deadline
//! wrapper, and the drained-daemon probe.
//!
//! Since the terminald split (`docs/terminald-split-design.md`) Horizon runs
//! *two* of these runtimes — one per daemon, each with its own connection,
//! op queue, and [`RuntimeControl`] — so that draining one leaves the other
//! untouched. Everything in this module is deliberately domain-free: the
//! agent-specific and terminal-specific halves live in [`super::agent`]
//! and [`super::terminal`] respectively, because their op vocabularies,
//! their `hello` replies, and what a drain costs genuinely differ (agentd's
//! drain kills nothing; terminald's kills every PTY, which is also why only
//! terminald carries the below-schema skew insurance).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use horizon_wire::{HubError, WireCodec, RTC_MAX_REPLY_BYTES, RTC_MAX_REQUEST_BYTES};
use remoc::rtc;
use remoc::RemoteSend;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// The bound on the whole remoc establishment sequence — chmux handshake,
/// base-channel handover, and the `hello` rtc call. A healthy daemon
/// completes it in milliseconds; what this bounds is the *cross-generation*
/// case (`docs/remoc-adoption-design.md` §6): a still-running JSONL
/// daemon blocks in `read_line` waiting for a newline our chmux hello
/// never contains (measured — the v9 pre-hello loop is *silent* against
/// chmux bytes), so it presents as this timeout — never chmux's own raw
/// 60 s `ChMux(Timeout)`.
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Test-only override for [`ESTABLISH_TIMEOUT`]
/// (`HORIZON_TEST_ESTABLISH_TIMEOUT_MS`): the silence-escalation tests
/// would otherwise take `SILENCE_MISMATCH_THRESHOLD × 5 s` of real wall
/// clock per stale daemon. Never set in production.
pub(super) fn establish_timeout() -> Duration {
    std::env::var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(ESTABLISH_TIMEOUT)
}

/// How many *consecutive* silent establish timeouts equal "a stale daemon
/// generation holds the socket" (`docs/remoc-adoption-design.md` §6's
/// bounded-timeout detection). One timeout is not evidence — a healthy
/// daemon can be transiently unresponsive (its one-at-a-time accept loop
/// busy, host under load) and the once-per-runtime recovery budget must not
/// be burned on that — but a *v9 daemon is silent every time* (measured:
/// its pre-hello `read_line` never completes on chmux bytes), so
/// persistence is the signal.
pub(super) const SILENCE_MISMATCH_THRESHOLD: u32 = 3;

/// Deadline for one established-phase rtc call (`list_terminals`,
/// `list_agents`, `attach_terminal`, `attach_agent`, `new_agent`). Not
/// tight on purpose: `list_agents`/`new_agent`/`attach_agent` legitimately
/// block on the daemon's resume-readiness gate (a large event log takes
/// real seconds to resume), so a short deadline would misreport a healthy
/// startup as a failure. A timeout fails only that op — the runtime and
/// connection survive.
pub(super) const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-probe budget for a drained daemon's process to actually exit,
/// observed as its socket refusing connections -- the same signal (and the
/// same 2s budget) as `super::wait_for_drain`, which the explicit reload
/// commands use.
const DRAIN_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// One daemon connection's control block: cancellation and establishment
/// state. Created per runtime instance and replaced wholesale when that
/// runtime is reloaded, so a reload simply reads the fresh control.
///
/// The negotiated version number is deliberately *not* kept here. Under
/// lockstep versioning it carries no information a client decision could
/// use — whatever this build negotiates with speaks this build's wire — so
/// the last reader of it went with the structured-input check
/// (`docs/runtime-crate-alignment-design.md` phase 3). A dead connection is
/// surfaced through reachability (the per-pane `RuntimeReachability`, plus
/// the pane dropping its window when the channels close).
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

    pub(super) async fn cancelled(&self) {
        let notified = self.notify.notified();
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    pub(super) fn mark_established(&self) {
        self.established.store(true, Ordering::Release);
    }

    pub(super) fn mark_stopped(&self) {
        let (lock, wake) = &self.stopped;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}

pub(super) enum EstablishError {
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

pub(super) enum StreamEnd {
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

/// How a failed establishment enters the reconnect loop's vocabulary. The
/// mapping is one-to-one by construction — [`EstablishError`] is what the
/// establish legs can fail with, [`StreamEnd`] is what both runtimes'
/// reconnect loops dispatch on — and each [`EstablishError`] variant's doc
/// names its counterpart, so the two types must stay in step.
impl From<EstablishError> for StreamEnd {
    fn from(error: EstablishError) -> Self {
        match error {
            EstablishError::Transient(message) => StreamEnd::PreHelloTransport { message },
            EstablishError::Silence(message) => StreamEnd::Silence { message },
            EstablishError::Garbage(message) => StreamEnd::GenerationMismatch { message },
            EstablishError::Rejected(message) => StreamEnd::VersionRejected { message },
            EstablishError::Fatal(message) => StreamEnd::Fatal(message),
        }
    }
}

/// Classifies a failed `Connect::io`: framing/protocol violations are
/// positive "the peer is not speaking chmux" evidence (measured: a JSONL
/// line read as a chmux length prefix fails instantly with a
/// `LengthDelimitedCodecError` under `ErrorKind::InvalidData`; a decodable
/// frame with an invalid multiplex message is `ChMuxError::Protocol`);
/// everything else — closes, resets, plain IO errors — is transient.
pub(super) fn classify_connect_error(
    daemon: &str,
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
            "{daemon} sent bytes that are not remoc/chmux (likely a stale daemon binary): {error}"
        ))
    } else {
        EstablishError::Transient(format!("remoc connect to {daemon} failed: {error}"))
    }
}

/// The chmux multiplexer task, which must be polled for the connection to
/// make any progress (adoption condition 3) and aborted once the connection
/// ends -- including on every failure path, so a timed-out establishment
/// never leaks a task still holding the socket.
pub(super) type ConnTask =
    JoinHandle<Result<(), remoc::chmux::ChMuxError<std::io::Error, std::io::Error>>>;

/// What [`connect_hub`] hands back: the daemon's hub client, its multiplexer
/// task, and what is left of the establish budget for the caller's `hello`.
pub(super) struct Connected<C> {
    pub(super) hub: C,
    pub(super) conn_task: ConnTask,
    /// The one deadline the whole establishment shares — the caller's
    /// `hello` leg rides the remainder of it.
    pub(super) deadline: tokio::time::Instant,
    /// The full budget behind that deadline, for the caller's own
    /// "no answer within {timeout:?}" message.
    pub(super) timeout: Duration,
}

/// The client half of [`horizon_wire::daemon::serve_connection`], and the
/// only part of an establishment that is genuinely domain-free: connect
/// remoc over `stream`, take the hub client the daemon hands over on the
/// base channel, and set the rtc size caps. Both runtimes then diverge at
/// their own `hello` — a different call on a different hub, answered by a
/// different reply — which is why this stops one leg short of it.
///
/// Every leg is bounded by one shared deadline ([`establish_timeout`], the
/// client-side counterpart of the server's `CONNECT_TIMEOUT`): a daemon
/// generation that cannot speak chmux presents as silence here rather than
/// as chmux's own raw 60 s timeout.
pub(super) async fn connect_hub<S, C>(
    stream: S,
    daemon: &str,
) -> Result<Connected<C>, EstablishError>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
    C: RemoteSend + rtc::Client,
{
    let timeout = establish_timeout();
    let deadline = tokio::time::Instant::now() + timeout;
    let (read_half, write_half) = tokio::io::split(stream);

    let connect =
        remoc::Connect::io::<_, _, (), C, WireCodec>(remoc::Cfg::default(), read_half, write_half);
    let (conn, _base_tx, mut base_rx) = match tokio::time::timeout_at(deadline, connect).await {
        Ok(Ok(connected)) => connected,
        Ok(Err(error)) => return Err(classify_connect_error(daemon, &error)),
        Err(_elapsed) => {
            return Err(EstablishError::Silence(format!(
                "{daemon} sent no remoc handshake within {timeout:?}"
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
            return Err(EstablishError::Transient(format!(
                "{daemon} closed the connection before handing over its hub client"
            )));
        }
        Err(_elapsed) => {
            conn_task.abort();
            return Err(EstablishError::Silence(format!(
                "{daemon} handed over no hub client within {timeout:?}"
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

    Ok(Connected {
        hub,
        conn_task,
        deadline,
        timeout,
    })
}

/// Bounds one established-phase rtc call. A deadline expiry fails only
/// that call (the reply channel gets an error, or the routes get a
/// per-session failure) — the runtime and the connection stay up, because
/// a wedged single call must not take down every other live attachment.
pub(super) async fn with_deadline<T>(
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

/// True once `socket_path` refuses connections (the daemon process is
/// gone -- its drain exit leaves the socket file behind, so file
/// existence proves nothing); false if it still accepts when
/// [`DRAIN_EXIT_TIMEOUT`] runs out.
pub(super) async fn wait_until_refusing(socket_path: &std::path::Path) -> bool {
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
