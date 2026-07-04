//! Horizon-side client for `horizon-agentd`: spawn-or-connect (decision 4 in
//! `docs/agent-runtime-split-design.md`) and the `hello` handshake, with a
//! contract-version mismatch surfaced as a plain `String` error rather than
//! silently ignored (the design's replay/reconnect section calls this out
//! by name: "surfaced to the user as reload required").
//!
//! Gated behind `[agent].agentd` in Horizon's config file (default `false`,
//! see [`agentd_enabled`]) -- production agent sessions stay fully
//! in-process until step 4 wires this connection into anything. This
//! module is therefore a `pub(crate)` API exercised by its own tests plus
//! one real (but inert-by-default) call site, [`maybe_connect_at_startup`],
//! rather than anything users can currently observe.
//!
//! Horizon has no async runtime of its own (floem drives its own event
//! loop, not tokio); [`maybe_connect_at_startup`] spins up a throwaway
//! current-thread tokio runtime on a background OS thread so a slow or
//! failing `horizon-agentd` never blocks window startup.

use std::path::Path;
use std::time::Duration;

use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, Hello, CONTRACT_VERSION};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixStream;

const RETRY_ATTEMPTS: u32 = 40;
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Whether Horizon should attempt to connect to `horizon-agentd` at all --
/// mirrors `[agent].agentd` in the config file (default `false`).
pub(crate) fn agentd_enabled() -> bool {
    crate::config::load().agent.agentd
}

/// Best-effort, fire-and-forget connection attempt at startup, only when
/// [`agentd_enabled`]. Logs the outcome to stderr; nothing else observes it
/// yet (see the module doc) -- this exists so the plumbing in this module
/// is exercised end to end (including the actual spawn-or-connect dance)
/// ahead of step 4 wiring it to anything real.
pub(crate) fn maybe_connect_at_startup() {
    if !agentd_enabled() {
        return;
    }
    let socket_path = horizon_agent::socket::default_socket_path();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                eprintln!("horizon: could not start a runtime for horizon-agentd: {err}");
                return;
            }
        };
        match runtime.block_on(connect(&socket_path)) {
            Ok(hello) => eprintln!(
                "horizon: connected to horizon-agentd (binary_id={})",
                hello.binary_id
            ),
            Err(err) => eprintln!("horizon: could not connect to horizon-agentd: {err}"),
        }
    });
}

/// Connects to `horizon-agentd` at `socket_path` (spawning it if nothing is
/// listening yet) and completes the hello handshake, returning agentd's
/// [`Hello`] on success or a human-readable error string -- a version
/// mismatch or an explicit [`Control::HandshakeRejected`] included.
pub(crate) async fn connect(socket_path: &Path) -> Result<Hello, String> {
    let stream = connect_or_spawn(socket_path).await?;
    handshake(stream).await
}

async fn connect_or_spawn(socket_path: &Path) -> Result<UnixStream, String> {
    if let Ok(stream) = UnixStream::connect(socket_path).await {
        return Ok(stream);
    }
    spawn_agentd(socket_path)?;
    retry_connect(socket_path).await
}

fn spawn_agentd(socket_path: &Path) -> Result<(), String> {
    std::process::Command::new("horizon-agentd")
        .arg("--socket")
        .arg(socket_path)
        .spawn()
        .map(|_child| ())
        .map_err(|err| format!("failed to spawn horizon-agentd: {err}"))
}

async fn retry_connect(socket_path: &Path) -> Result<UnixStream, String> {
    for _ in 0..RETRY_ATTEMPTS {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(RETRY_DELAY).await,
        }
    }
    Err(format!(
        "timed out waiting for horizon-agentd to accept connections on {}",
        socket_path.display()
    ))
}

/// The hello exchange itself, generic over `AsyncRead + AsyncWrite` (same
/// framing-over-any-stream guardrail `horizon_agent::wire` follows) so it's
/// directly testable over `tokio::io::duplex` without a real socket -- see
/// this module's tests.
async fn handshake<S>(stream: S) -> Result<Hello, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    let our_hello = Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: Vec::new(),
    };
    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(our_hello)),
    )
    .await
    .map_err(|err| format!("failed to send hello to horizon-agentd: {err}"))?;

    let envelope = wire::read_envelope(&mut reader)
        .await
        .map_err(|err| format!("failed to read horizon-agentd's hello reply: {err}"))?
        .ok_or_else(|| {
            "horizon-agentd closed the connection before replying to hello".to_string()
        })?;

    match envelope.body {
        EnvelopeBody::Control(Control::Hello(hello))
            if hello.contract_version == CONTRACT_VERSION =>
        {
            Ok(hello)
        }
        EnvelopeBody::Control(Control::Hello(hello)) => Err(format!(
            "horizon-agentd contract version mismatch: horizon speaks v{CONTRACT_VERSION}, \
             agentd speaks v{} -- reload required",
            hello.contract_version
        )),
        EnvelopeBody::Control(Control::HandshakeRejected(reason)) => {
            Err(format!("horizon-agentd rejected the handshake: {reason}"))
        }
        other => Err(format!(
            "horizon-agentd sent an unexpected reply to hello: {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fake_agentd_reply(server_side: tokio::io::DuplexStream, reply: Envelope) {
        let (read_half, mut write_half) = tokio::io::split(server_side);
        let mut reader = BufReader::new(read_half);
        wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("client should send a hello");
        wire::write_envelope(&mut write_half, &reply).await.unwrap();
    }

    #[tokio::test]
    async fn handshake_succeeds_when_the_peer_answers_with_a_matching_hello() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_agentd_reply(
            server_side,
            Envelope::control(Control::Hello(Hello {
                contract_version: CONTRACT_VERSION,
                binary_id: "test-agentd".to_string(),
                capabilities: vec![],
            })),
        ));

        let hello = handshake(client_side)
            .await
            .expect("handshake should succeed");
        server.await.unwrap();

        assert_eq!(hello.binary_id, "test-agentd");
    }

    #[tokio::test]
    async fn handshake_surfaces_a_contract_version_mismatch_as_an_error_string() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_agentd_reply(
            server_side,
            Envelope::control(Control::Hello(Hello {
                contract_version: CONTRACT_VERSION + 1,
                binary_id: "stale-agentd".to_string(),
                capabilities: vec![],
            })),
        ));

        let error = handshake(client_side).await.unwrap_err();
        server.await.unwrap();

        assert!(error.contains("reload required"), "error was: {error}");
    }

    #[tokio::test]
    async fn handshake_surfaces_a_rejection_reason_as_an_error_string() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_agentd_reply(
            server_side,
            Envelope::control(Control::HandshakeRejected("nope".to_string())),
        ));

        let error = handshake(client_side).await.unwrap_err();
        server.await.unwrap();

        assert!(error.contains("nope"), "error was: {error}");
    }

    #[tokio::test]
    async fn handshake_surfaces_a_connection_closed_before_reply_as_an_error_string() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        // Reads the hello (so the client's write is guaranteed to succeed),
        // then drops both split halves without replying -- deterministically
        // exercising the "closed mid-handshake" path on the client's *read*,
        // not a racy failure on its write.
        let server = tokio::spawn(async move {
            let (read_half, _write_half) = tokio::io::split(server_side);
            let mut reader = BufReader::new(read_half);
            wire::read_envelope(&mut reader)
                .await
                .unwrap()
                .expect("client should send a hello");
        });

        let error = handshake(client_side).await.unwrap_err();
        server.await.unwrap();

        assert!(error.contains("closed"), "error was: {error}");
    }
}
