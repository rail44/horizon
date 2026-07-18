//! Horizon-side client for `horizon-sessiond`: spawn-or-connect (decision 4 in
//! `docs/agent-runtime-split-design.md`) and the `hello` handshake, with a
//! contract-version mismatch surfaced as a plain `String` error rather than
//! silently ignored (the design's replay/reconnect section calls this out
//! by name: "surfaced to the user as reload required").
//!
//! `horizon-sessiond` is the *only* place agent sessions run -- there is no
//! in-process fallback or daemon feature flag. The shared `src/sessiond`
//! runtime uses [`connect_or_spawn_retrying`] to obtain the raw stream, then
//! owns the cross-domain handshake and multiplexing itself.
//!
//! Horizon has no process-wide Tokio runtime; `src/sessiond` owns a dedicated
//! current-thread runtime on a background OS thread so a slow or failing
//! daemon never blocks window startup.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
use horizon_session_protocol::{
    self as session_wire, Hello, SessionControl, SESSION_CONTROL_KIND,
    SESSION_PROTOCOL_VERSION as CONTRACT_VERSION,
};
#[cfg(test)]
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixStream;

/// Starting delay for [`connect_or_spawn_retrying`]'s exponential backoff
/// (doubling, capped at 1s -- see that function). Verified still generous
/// after `horizon-sessiond`'s bind-first startup fix (it binds the socket
/// as its first action, before reading its event log or resuming any
/// session -- see that binary's `main` module doc): a freshly spawned
/// sessiond's `connect` now succeeds within milliseconds of process start
/// regardless of event-log size, since nothing before `bind_listener`
/// touches the log.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// The binary name `horizon-sessiond` is spawned as/looked up as -- see
/// [`resolve_sessiond_binary`].
const SESSIOND_BINARY_NAME: &str = "horizon-sessiond";

/// Connects immediately when sessiond is already listening; otherwise starts
/// it once and keeps retrying with capped backoff until its socket is ready.
/// The Horizon-side shared runtime owns the handshake and all routing after
/// this returns.
pub async fn connect_or_spawn_retrying(
    socket_path: &Path,
    control_socket: &Path,
) -> Result<UnixStream, String> {
    if let Ok(stream) = UnixStream::connect(socket_path).await {
        return Ok(stream);
    }
    spawn_sessiond(socket_path, control_socket)?;

    let mut delay = RETRY_DELAY;
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(delay).await,
        }
        delay = (delay * 2).min(Duration::from_secs(1));
    }
}

fn spawn_sessiond(socket_path: &Path, control_socket: &Path) -> Result<(), String> {
    let binary = resolve_sessiond_binary();
    sessiond_command(&binary, socket_path, control_socket)
        .spawn()
        .map(|_child| ())
        .map_err(|err| {
            format!(
                "failed to spawn {} ({err}) -- run `cargo build --workspace` to build \
                 horizon-sessiond, then try again",
                binary.display()
            )
        })
}

/// Builds the `horizon-sessiond --socket <path>` command [`spawn_sessiond`]
/// spawns, injecting `HORIZON_SOCKET` into its environment so sessiond's own
/// `bash` tool (and anything else a session might shell out to) defaults to
/// targeting *this* Horizon instance's control socket --
/// `docs/cli-control-plane-design.md`'s "Discovery" decision. Split out from
/// `spawn_sessiond` so the env injection is directly assertable without
/// actually spawning a process (see this module's tests).
fn sessiond_command(
    binary: &Path,
    socket_path: &Path,
    control_socket: &Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(binary);
    command
        .arg("--socket")
        .arg(socket_path)
        .env("HORIZON_SOCKET", control_socket);
    command
}

/// Where to look for the `horizon-sessiond` binary: first, right next to
/// Horizon's own executable (the shape `cargo build --workspace`/`cargo run`
/// produces -- both binaries land in the same `target/debug` or
/// `target/release` directory), falling back to a bare name resolved
/// through `PATH` (an installed deployment, or a developer who's put it
/// there themselves). The dev-flow gotcha this exists for: `cargo run`
/// alone only rebuilds the `horizon` binary, and `target/debug` is not on
/// `PATH` by default, so a bare `Command::new("horizon-sessiond")` would
/// reliably fail to find a workspace build even though one exists two
/// directories away -- see [`spawn_sessiond`]'s error message for the
/// resulting actionable hint when neither location has it.
fn resolve_sessiond_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(SESSIOND_BINARY_NAME);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(SESSIOND_BINARY_NAME)
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
/// `AsyncWrite` pair rather than owning the whole stream. `handshake` above
/// is the same exchange over an owned, not-yet-split stream, kept for this
/// module's tests (which construct a single `tokio::io::duplex` stream
/// directly) so they don't need to juggle split halves themselves; this is
/// the split-halves version `handshake` itself delegates to. Test-only:
/// production connects via [`connect_or_spawn_retrying`], and `src/sessiond`
/// owns its own independent handshake over the resulting stream (see this
/// module's doc comment).
#[cfg(test)]
async fn handshake_over<R, W>(reader: &mut R, writer: &mut W) -> Result<Hello, String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let our_hello = Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: concat!("horizon/", env!("CARGO_PKG_VERSION")).to_string(),
        capabilities: Vec::new(),
    };
    let hello_envelope = session_wire::Envelope::session_control(&SessionControl::Hello(our_hello))
        .map_err(|err| format!("failed to encode hello for horizon-sessiond: {err}"))?;
    session_wire::write_envelope(writer, &hello_envelope)
        .await
        .map_err(|err| format!("failed to send hello to horizon-sessiond: {err}"))?;

    let envelope = session_wire::read_envelope(reader)
        .await
        .map_err(|err| format!("failed to read horizon-sessiond's hello reply: {err}"))?
        .ok_or_else(|| {
            "horizon-sessiond closed the connection before replying to hello".to_string()
        })?;

    let control: SessionControl = envelope
        .decode_payload(SESSION_CONTROL_KIND)
        .map_err(|err| format!("failed to decode horizon-sessiond's hello reply: {err}"))?;
    match control {
        SessionControl::Hello(hello) if hello.contract_version == CONTRACT_VERSION => Ok(hello),
        SessionControl::Hello(hello) => Err(format!(
            "horizon-sessiond contract version mismatch: horizon speaks v{CONTRACT_VERSION}, \
             sessiond speaks v{} -- reload required",
            hello.contract_version
        )),
        SessionControl::HandshakeRejected(reason) => {
            Err(format!("horizon-sessiond rejected the handshake: {reason}"))
        }
        other => Err(format!(
            "horizon-sessiond sent an unexpected reply to hello: {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessiond_command_injects_the_control_socket_env_var() {
        let command = sessiond_command(
            Path::new("/usr/bin/horizon-sessiond"),
            Path::new("/tmp/x.sock"),
            Path::new("/tmp/horizon-control-test.sock"),
        );

        let value = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("HORIZON_SOCKET"))
            .and_then(|(_, value)| value);

        assert_eq!(
            value,
            Some(std::ffi::OsStr::new("/tmp/horizon-control-test.sock"))
        );
    }

    async fn fake_sessiond_reply(server_side: tokio::io::DuplexStream, reply: SessionControl) {
        let (read_half, mut write_half) = tokio::io::split(server_side);
        let mut reader = BufReader::new(read_half);
        let hello = session_wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("client should send a hello");
        let control: SessionControl = hello.decode_payload(SESSION_CONTROL_KIND).unwrap();
        assert!(matches!(
            control,
            SessionControl::Hello(Hello { binary_id, .. })
                if binary_id == concat!("horizon/", env!("CARGO_PKG_VERSION"))
        ));
        let reply = session_wire::Envelope::session_control(&reply).unwrap();
        session_wire::write_envelope(&mut write_half, &reply)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn handshake_succeeds_when_the_peer_answers_with_a_matching_hello() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_sessiond_reply(
            server_side,
            SessionControl::Hello(Hello {
                contract_version: CONTRACT_VERSION,
                binary_id: "test-sessiond".to_string(),
                capabilities: vec![],
            }),
        ));

        let hello = handshake(client_side)
            .await
            .expect("handshake should succeed");
        server.await.unwrap();

        assert_eq!(hello.binary_id, "test-sessiond");
    }

    #[tokio::test]
    async fn handshake_surfaces_a_contract_version_mismatch_as_an_error_string() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_sessiond_reply(
            server_side,
            SessionControl::Hello(Hello {
                contract_version: CONTRACT_VERSION + 1,
                binary_id: "stale-sessiond".to_string(),
                capabilities: vec![],
            }),
        ));

        let error = handshake(client_side).await.unwrap_err();
        server.await.unwrap();

        assert!(error.contains("reload required"), "error was: {error}");
    }

    #[tokio::test]
    async fn handshake_surfaces_a_rejection_reason_as_an_error_string() {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let server = tokio::spawn(fake_sessiond_reply(
            server_side,
            SessionControl::HandshakeRejected("nope".to_string()),
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
            session_wire::read_envelope(&mut reader)
                .await
                .unwrap()
                .expect("client should send a hello");
        });

        let error = handshake(client_side).await.unwrap_err();
        server.await.unwrap();

        assert!(error.contains("closed"), "error was: {error}");
    }
}
