//! Horizon-side client for `horizon-agentd`: spawn-or-connect (decision 4 in
//! `docs/agent-runtime-split-design.md`) and the `hello` handshake, with a
//! contract-version mismatch surfaced as a plain `String` error rather than
//! silently ignored (the design's replay/reconnect section calls this out
//! by name: "surfaced to the user as reload required").
//!
//! Gated behind `[agent].agentd` in Horizon's config file (default `false`,
//! see [`agentd_enabled`]). As of step 3, [`connect_and_split`] is the real
//! production entry point -- `agent::agentd_runtime::AgentdConnection::
//! connect` calls it to get a live, handshaken connection it then keeps
//! multiplexing sessions over; this module itself stays limited to
//! connect-or-spawn plus the handshake.
//!
//! Horizon has no async runtime of its own (floem drives its own event
//! loop, not tokio); callers that need to run this module's `async` fns
//! from plain (non-async) Horizon code spin up their own throwaway tokio
//! runtime on a background OS thread (see `agentd_runtime::AgentdConnection
//! ::connect`) so a slow or failing `horizon-agentd` never blocks window
//! startup.

use std::path::Path;
use std::time::Duration;

use floem::prelude::RwSignal;
use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, Hello, CONTRACT_VERSION};
#[cfg(test)]
use tokio::io::AsyncRead;
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::workspace::Workspace;

const RETRY_ATTEMPTS: u32 = 40;
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Whether Horizon should attempt to connect to `horizon-agentd` at all --
/// mirrors `[agent].agentd` in the config file (default `false`).
pub(crate) fn agentd_enabled() -> bool {
    crate::config::load().agent.agentd
}

/// Connects to `horizon-agentd` at startup when [`agentd_enabled`], wiring
/// up the host-tool responder (`agentd_runtime::wire_host_tool_responder`)
/// against `workspace` on success. The one production call site is
/// `app::state::AppState::new`, which stores the result and threads it into
/// every agent session's spawn path (`app::runtime::SessionRuntimeState`).
///
/// Returns `None` both when the flag is off (the default -- byte-for-byte
/// unchanged behavior) and when the connection attempt failed (logged to
/// stderr): either way, `app::runtime::agent::spawn_agent_session` falls
/// back to running every session fully in-process, exactly as if agentd
/// didn't exist.
pub(crate) fn connect_agentd_at_startup(
    workspace: RwSignal<Workspace>,
) -> Option<AgentdConnection> {
    if !agentd_enabled() {
        return None;
    }
    let socket_path = horizon_agent::socket::default_socket_path();
    match AgentdConnection::connect(&socket_path) {
        Ok((connection, host_tool_requests)) => {
            eprintln!("horizon: connected to horizon-agentd");
            crate::agent::agentd_runtime::wire_host_tool_responder(
                connection.clone(),
                host_tool_requests,
                workspace,
            );
            Some(connection)
        }
        Err(err) => {
            eprintln!(
                "horizon: could not connect to horizon-agentd ({err}); agent sessions will run \
                 in-process for this run"
            );
            None
        }
    }
}

/// Connects to `horizon-agentd` at `socket_path` (spawning it if nothing is
/// listening yet), completes the hello handshake, and hands back the split
/// halves ready for the session-hosting traffic that follows a successful
/// handshake -- the production entry point `agentd_runtime::AgentdConnection
/// ::connect` builds its read/write tasks on top of.
pub(crate) async fn connect_and_split(
    socket_path: &Path,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Hello), String> {
    let stream = connect_or_spawn(socket_path).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let hello = handshake_over(&mut reader, &mut write_half).await?;
    Ok((reader, write_half, hello))
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
#[cfg(test)]
async fn handshake<S>(stream: S) -> Result<Hello, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    handshake_over(&mut reader, &mut write_half).await
}

/// The hello exchange itself, generic over an already-split `AsyncBufRead`/
/// `AsyncWrite` pair rather than owning the whole stream -- so a caller that
/// needs the connection to keep living past a successful handshake (
/// [`connect_and_split`], which hands the same halves back to its caller)
/// doesn't lose them the way owning-and-splitting internally would. `handshake`
/// above is the same exchange over an owned, not-yet-split stream, kept for
/// this module's tests (which construct a single `tokio::io::duplex` stream
/// directly) so they don't need to juggle split halves themselves.
async fn handshake_over<R, W>(reader: &mut R, writer: &mut W) -> Result<Hello, String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let our_hello = Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: Vec::new(),
    };
    wire::write_envelope(writer, &Envelope::control(Control::Hello(our_hello)))
        .await
        .map_err(|err| format!("failed to send hello to horizon-agentd: {err}"))?;

    let envelope = wire::read_envelope(reader)
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
