//! What both daemon client runtimes share: the per-runtime control handle,
//! the establishment deadlines and their classification, the per-call
//! deadline wrapper, and the drained-daemon probe.
//!
//! Since the terminald split (`docs/terminald-split-design.md`) Horizon runs
//! *two* of these runtimes — one per daemon, each with its own connection,
//! op queue, and [`RuntimeControl`] — so that draining one leaves the other
//! untouched. Everything in this module is deliberately domain-free: the
//! agent-specific and terminal-specific halves live in [`super::connection`]
//! and [`super::terminald`] respectively, because their op vocabularies,
//! their `hello` replies, and their recovery paths genuinely differ (only
//! sessiond has a JSONL generation to recover from; only terminald carries
//! the below-schema skew insurance).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use horizon_session_protocol::HubError;
use tokio::sync::Notify;

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

/// One daemon connection's control block: cancellation, establishment
/// state, and the negotiated version. Created per runtime instance and
/// replaced wholesale when that runtime is reloaded, so a reload against a
/// different daemon version simply reads the fresh control.
pub(super) struct RuntimeControl {
    cancelled: AtomicBool,
    established: AtomicBool,
    /// The version `hello` negotiated, or `0` before the first establishment.
    /// Read live by terminal panes to gate the v12 scrollback windowing
    /// surface (`docs/terminal-scrollback-design.md` §4): a pane only sends
    /// `RequestScrollWindow` when this is ≥ 12, otherwise it keeps today's
    /// round-trip `Scroll`. Set once per establishment (alongside
    /// `mark_established`) and **not cleared on disconnect** — it holds the
    /// most recently negotiated version until the next establishment
    /// overwrites it. A dead connection is surfaced through reachability (the
    /// per-pane `RuntimeReachability`, plus the pane dropping its window when
    /// the channels close), not by zeroing this.
    negotiated: AtomicU32,
    notify: Notify,
    stopped: (Mutex<bool>, Condvar),
}

impl RuntimeControl {
    pub(super) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            established: AtomicBool::new(false),
            negotiated: AtomicU32::new(0),
            notify: Notify::new(),
            stopped: (Mutex::new(false), Condvar::new()),
        }
    }

    /// The most recently negotiated protocol version, or `None` before the
    /// first establishment. `None` and any value below 12 both gate the
    /// scrollback windowing surface off (conservative: never send a window
    /// request a peer might not answer).
    pub(super) fn negotiated(&self) -> Option<u32> {
        match self.negotiated.load(Ordering::Acquire) {
            0 => None,
            version => Some(version),
        }
    }

    pub(super) fn set_negotiated(&self, version: u32) {
        self.negotiated.store(version, Ordering::Release);
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
