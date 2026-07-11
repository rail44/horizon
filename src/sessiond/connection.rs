use std::path::PathBuf;
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
        if let StreamEnd::Fatal(error) | StreamEnd::EstablishedFailure(error) = end {
            routes.connection_failed(error);
        }
        control.mark_stopped();
    });
}

enum StreamEnd {
    PreHelloTransport(String),
    Fatal(String),
    EstablishedFailure(String),
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
}

async fn handshake<R, W>(reader: &mut R, writer: &mut W) -> Result<(), HandshakeError>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = RawEnvelope::session_control(&SessionControl::Hello(Hello {
        contract_version: SESSION_PROTOCOL_VERSION,
        binary_id: concat!("horizon/", env!("CARGO_PKG_VERSION")).to_string(),
        capabilities: vec!["agent".to_string(), "terminal".to_string()],
    }))
    .map_err(|error| HandshakeError::Fatal(format!("failed to encode sessiond hello: {error}")))?;
    session_wire::write_envelope(writer, &hello)
        .await
        .map_err(|error| {
            HandshakeError::Transport(format!("failed to send sessiond hello: {error}"))
        })?;

    let reply = match session_wire::read_envelope(reader).await {
        Ok(Some(reply)) => reply,
        Ok(None) => {
            return Err(HandshakeError::Transport(
                "sessiond disconnected before replying to hello".to_string(),
            ))
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
        SessionControl::Hello(hello) => Err(HandshakeError::Fatal(format!(
            "sessiond contract version mismatch: horizon speaks v{SESSION_PROTOCOL_VERSION}, \
             sessiond speaks v{} -- reload required",
            hello.contract_version
        ))),
        SessionControl::HandshakeRejected(reason) => Err(HandshakeError::Fatal(format!(
            "sessiond rejected the handshake: {reason}"
        ))),
        other => Err(HandshakeError::Fatal(format!(
            "sessiond sent an unexpected hello reply: {other:?}"
        ))),
    }
}
