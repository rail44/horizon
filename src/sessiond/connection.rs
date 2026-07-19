use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use horizon_session_protocol::{
    self as session_wire, Envelope as RawEnvelope, Hello, SessionControl, SESSION_CONTROL_KIND,
    SESSION_PROTOCOL_VERSION,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, WeakUnboundedSender};
use tokio::sync::Notify;

use super::routing::{Incoming, Routes};

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

pub(super) fn spawn(
    socket_path: PathBuf,
    control_socket: PathBuf,
    mut outgoing: UnboundedReceiver<RawEnvelope>,
    weak_outgoing: WeakUnboundedSender<RawEnvelope>,
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
            let mut hello_retry_delay = Duration::from_millis(50);
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

                match run_stream(
                    stream,
                    &mut outgoing,
                    weak_outgoing.clone(),
                    routes.clone(),
                    control.clone(),
                )
                .await
                {
                    StreamEnd::PreHelloTransport(error) => {
                        eprintln!("horizon-sessiond hello transport failed, retrying: {error}");
                        tokio::select! {
                            _ = tokio::time::sleep(hello_retry_delay) => {}
                            _ = control.cancelled() => {
                                routes.connection_failed("sessiond runtime stopped".to_string());
                                break;
                            }
                        }
                        hello_retry_delay = (hello_retry_delay * 2).min(Duration::from_secs(1));
                    }
                    StreamEnd::VersionMismatch {
                        daemon_version,
                        message,
                    } => {
                        // Auto-recovery (docs/session-daemon-design.md,
                        // 2026-07-20): drain the stale daemon gracefully and
                        // let the next iteration's connect_or_spawn_retrying
                        // start a fresh one. Attempted exactly once per
                        // runtime: if the respawned daemon still mismatches
                        // (a stale horizon-sessiond binary -- `cargo run`
                        // rebuilds only the horizon binary), restarting it
                        // again would loop forever, so give up loudly
                        // instead.
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
                        match daemon_version {
                            Some(version) => eprintln!(
                                "horizon-sessiond v{version} detected (horizon speaks \
                                 v{SESSION_PROTOCOL_VERSION}), draining and restarting it"
                            ),
                            None => eprintln!(
                                "a horizon-sessiond speaking an unknown older contract version \
                                 detected (horizon speaks v{SESSION_PROTOCOL_VERSION}), draining \
                                 and restarting it"
                            ),
                        }
                        let drained = tokio::select! {
                            drained = drain_stale_sessiond(&socket_path, daemon_version) => drained,
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
    mut outgoing: UnboundedReceiver<RawEnvelope>,
    weak_outgoing: WeakUnboundedSender<RawEnvelope>,
    routes: Arc<Routes>,
    control: Arc<RuntimeControl>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let end = runtime.block_on(run_stream(
            stream,
            &mut outgoing,
            weak_outgoing,
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
            StreamEnd::VersionMismatch { message, .. } => routes.connection_failed(message),
            StreamEnd::PreHelloTransport(_) | StreamEnd::Cancelled | StreamEnd::Dropped => {}
        }
        control.mark_stopped();
    });
}

enum StreamEnd {
    PreHelloTransport(String),
    Fatal(String),
    EstablishedFailure(String),
    /// The daemon on the socket speaks a different contract version --
    /// either it said so (`daemon_version` known) or it closed our hello
    /// without a reply, which is how a pre-v9 daemon (unable to decode a
    /// foreign-versioned envelope at all) presents (`daemon_version`
    /// unknown). Recoverable: see `spawn`'s drain-and-restart arm.
    VersionMismatch {
        daemon_version: Option<u32>,
        message: String,
    },
    Cancelled,
    Dropped,
}

async fn run_stream<S>(
    stream: S,
    outgoing: &mut UnboundedReceiver<RawEnvelope>,
    weak_outgoing: WeakUnboundedSender<RawEnvelope>,
    routes: Arc<Routes>,
    control: Arc<RuntimeControl>,
) -> StreamEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let handshake = tokio::select! {
        result = handshake(&mut reader, &mut writer) => result,
        _ = control.cancelled() => return StreamEnd::Cancelled,
    };
    match handshake {
        Ok(()) => control.mark_established(),
        Err(HandshakeError::Transport(error)) => return StreamEnd::PreHelloTransport(error),
        Err(HandshakeError::Fatal(error)) => return StreamEnd::Fatal(error),
        Err(HandshakeError::VersionMismatch {
            daemon_version,
            message,
        }) => {
            return StreamEnd::VersionMismatch {
                daemon_version,
                message,
            }
        }
    }

    loop {
        tokio::select! {
            _ = control.cancelled() => return StreamEnd::Cancelled,
            outgoing = outgoing.recv() => {
                let Some(envelope) = outgoing else {
                    return StreamEnd::Dropped;
                };
                if let Err(error) = session_wire::write_envelope(&mut writer, &envelope).await {
                    return StreamEnd::EstablishedFailure(format!(
                        "failed to write to established sessiond: {error}"
                    ));
                }
            }
            incoming = session_wire::read_envelope(&mut reader) => {
                let envelope = match incoming {
                    Ok(Some(envelope)) => envelope,
                    Ok(None) => return StreamEnd::EstablishedFailure(
                        "established sessiond disconnected".to_string()
                    ),
                    Err(error) => return StreamEnd::EstablishedFailure(format!(
                        "failed to read from established sessiond: {error}"
                    )),
                };
                match routes.dispatch(envelope) {
                    Ok(Incoming::Pong(pong)) => {
                        if let Some(outgoing) = weak_outgoing.upgrade() {
                            let _ = outgoing.send(pong);
                        }
                    }
                    Ok(Incoming::Handled) => {}
                    Err(error) => eprintln!("horizon-sessiond message ignored: {error}"),
                }
            }
        }
    }
}

enum HandshakeError {
    Transport(String),
    Fatal(String),
    /// See `StreamEnd::VersionMismatch`.
    VersionMismatch {
        daemon_version: Option<u32>,
        message: String,
    },
}

async fn handshake<R, W>(reader: &mut R, writer: &mut W) -> Result<(), HandshakeError>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = RawEnvelope::session_control(&SessionControl::Hello(Hello {
        contract_version: SESSION_PROTOCOL_VERSION,
        binary_id: concat!("horizon/", env!("CARGO_PKG_VERSION")).to_string(),
    }))
    .map_err(|error| HandshakeError::Fatal(format!("failed to encode sessiond hello: {error}")))?;
    session_wire::write_envelope(writer, &hello)
        .await
        .map_err(|error| {
            HandshakeError::Transport(format!("failed to send sessiond hello: {error}"))
        })?;

    let reply = match session_wire::read_envelope(reader).await {
        Ok(Some(reply)) => reply,
        // A clean close with no reply at all is how a pre-v9 daemon
        // presents a contract mismatch: it rejects any foreign-versioned
        // envelope before even looking at its kind, so it can neither
        // answer our hello nor tell us its own version. A daemon that
        // genuinely died in this window looks the same, but the recovery
        // path is harmless there (its socket already refuses connections,
        // so the drain is a no-op and the respawn is exactly what a dead
        // daemon needs).
        Ok(None) => {
            return Err(HandshakeError::VersionMismatch {
                daemon_version: None,
                message: format!(
                    "sessiond closed the connection without answering hello -- likely a stale \
                     daemon that cannot decode v{SESSION_PROTOCOL_VERSION} envelopes"
                ),
            })
        }
        Err(session_wire::WireError::Io(error)) => {
            return Err(HandshakeError::Transport(format!(
                "failed to read sessiond hello: {error}"
            )))
        }
        Err(session_wire::WireError::TornLine) => {
            return Err(HandshakeError::Transport(
                "sessiond disconnected during its hello reply".to_string(),
            ))
        }
        Err(session_wire::WireError::VersionMismatch { found, .. }) => {
            return Err(HandshakeError::VersionMismatch {
                daemon_version: Some(found),
                message: format!(
                    "sessiond contract version mismatch: horizon speaks \
                     v{SESSION_PROTOCOL_VERSION}, sessiond speaks v{found}"
                ),
            })
        }
        Err(error) => {
            return Err(HandshakeError::Fatal(format!(
                "failed to read sessiond hello: {error}"
            )))
        }
    };
    let control: SessionControl = reply
        .decode_payload(SESSION_CONTROL_KIND)
        .map_err(|error| {
            HandshakeError::Fatal(format!("failed to decode sessiond hello: {error}"))
        })?;
    match control {
        SessionControl::Hello(hello) if hello.contract_version == SESSION_PROTOCOL_VERSION => {
            Ok(())
        }
        SessionControl::Hello(hello) => Err(HandshakeError::VersionMismatch {
            daemon_version: Some(hello.contract_version),
            message: format!(
                "sessiond contract version mismatch: horizon speaks \
                 v{SESSION_PROTOCOL_VERSION}, sessiond speaks v{}",
                hello.contract_version
            ),
        }),
        // The daemon's only rejection today *is* the contract-version
        // check, and its reply envelope's `v` is the version it actually
        // speaks (the two constants are one re-export) -- so treat a
        // rejection as a recoverable mismatch rather than fatal. A future
        // rejection for some other reason costs one futile drain attempt
        // before the same message goes fatal.
        SessionControl::HandshakeRejected(reason) => Err(HandshakeError::VersionMismatch {
            daemon_version: Some(reply.v),
            message: format!("sessiond rejected the handshake: {reason}"),
        }),
        other => Err(HandshakeError::Fatal(format!(
            "sessiond sent an unexpected hello reply: {other:?}"
        ))),
    }
}

/// The earliest contract version whose `horizon-sessiond` honors a
/// pre-hello `SessionControl::Drain` (that handling landed together with
/// terminal hosting, in the v3 vocabulary). Daemons older than that predate
/// the `Drain` control entirely, so probing below it is pointless.
const OLDEST_DRAINABLE_VERSION: u32 = 3;

/// Per-probe budget for a drained daemon's process to actually exit,
/// observed as its socket refusing connections -- the same signal (and the
/// same 2s budget) as `super::wait_for_drain`, which the explicit `Reload
/// Session Runtime` flow uses.
const DRAIN_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Gracefully stops a version-mismatched daemon by sending it
/// `SessionControl::Drain` *at its own envelope version* on a fresh
/// connection -- the daemon's pre-hello loop has honored Drain since v3,
/// and a graceful drain (unlike a signal) flushes its event log before
/// exiting. When the daemon never told us its version (a pre-v9 daemon
/// closes a foreign-versioned hello without replying), probe downward from
/// the newest plausible stale version; a probe at the wrong version is
/// harmless (the daemon logs a malformed message and closes that one
/// connection), and a probe at the right one drains it.
///
/// Never probes at `SESSION_PROTOCOL_VERSION` itself: a healthy
/// same-version daemon must be unreachable from this path.
async fn drain_stale_sessiond(
    socket_path: &Path,
    daemon_version: Option<u32>,
) -> Result<(), String> {
    let candidates: Vec<u32> = match daemon_version {
        Some(version) => vec![version],
        None => (OLDEST_DRAINABLE_VERSION..SESSION_PROTOCOL_VERSION)
            .rev()
            .collect(),
    };
    for version in candidates {
        let mut stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(stream) => stream,
            // Nothing is accepting any more: either a previous probe's
            // drain just landed or the daemon died on its own. Done either
            // way -- the caller's next connect_or_spawn_retrying starts a
            // fresh daemon.
            Err(_) => return Ok(()),
        };
        let envelope = RawEnvelope::session_control_at(&SessionControl::Drain, version)
            .map_err(|error| format!("failed to encode a v{version} drain: {error}"))?;
        if session_wire::write_envelope(&mut stream, &envelope)
            .await
            .is_err()
        {
            continue;
        }
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

/// True once `socket_path` refuses connections (the daemon process is
/// gone -- its `Drain` exit leaves the socket file behind, so file
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
