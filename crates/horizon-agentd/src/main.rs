//! `horizon-agentd`: step 2 of `docs/agent-runtime-split-design.md`'s
//! agent-runtime split. Owns the Unix socket and the `hello` handshake;
//! agent sessions still run in-process inside Horizon at this step
//! (decision 5, step 2) -- `Command`/`Event` envelopes received here are
//! not yet acted on. What *is* real: `hello` (answered with this binary's
//! own version), `ping`/`pong`, `session_list` (always an empty list for
//! now), `drain` (flush + `exit(0)`), stale-socket recovery on bind, and a
//! clean shutdown on `SIGTERM` -- so step 3 can build session hosting on
//! top without redoing this plumbing.

use std::path::{Path, PathBuf};

use horizon_agent::config::{self, AgentConfig};
use horizon_agent::socket::default_socket_path;
use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, Hello, CONTRACT_VERSION};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Reported in this binary's `hello` reply's `binary_id` -- the crate
/// version, not the semantic "contract version" ([`CONTRACT_VERSION`],
/// carried separately in the same [`Hello`]).
const BINARY_ID: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path =
        socket_path_from_args(std::env::args().skip(1)).unwrap_or_else(default_socket_path);

    let file_config = config::load_file_config();
    let agent_config = AgentConfig::from_env_and_file(&file_config);
    eprintln!(
        "horizon-agentd: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    run(&socket_path).await
}

async fn run(socket_path: &Path) -> anyhow::Result<()> {
    let listener = bind_listener(socket_path).await?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = handle_connection(stream).await {
                    eprintln!("horizon-agentd: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-agentd: SIGTERM received, shutting down");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// One connection at a time by construction: [`run`]'s accept loop awaits
/// this to completion before accepting the next connection (multi-client
/// support is explicitly out of scope -- see the design doc's "Out of scope
/// here").
async fn handle_connection(stream: UnixStream) -> anyhow::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let envelope = match wire::read_envelope(&mut reader).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return Ok(()),
            Err(err) => {
                eprintln!("horizon-agentd: malformed message, closing connection: {err}");
                return Ok(());
            }
        };

        let EnvelopeBody::Control(control) = envelope.body else {
            // Command/Event envelopes aren't meaningful yet -- sessions
            // stay in-process inside Horizon until step 3 (decision 5).
            continue;
        };

        match control {
            Control::Hello(hello) => {
                if hello.contract_version != CONTRACT_VERSION {
                    let reason = format!(
                        "contract version mismatch: horizon-agentd speaks v{CONTRACT_VERSION}, \
                         client sent v{} -- reload required",
                        hello.contract_version
                    );
                    eprintln!("horizon-agentd: rejecting handshake: {reason}");
                    let _ = wire::write_envelope(
                        &mut writer,
                        &Envelope::control(Control::HandshakeRejected(reason)),
                    )
                    .await;
                    return Ok(());
                }
                wire::write_envelope(&mut writer, &our_hello_envelope()).await?;
            }
            Control::Ping => {
                wire::write_envelope(&mut writer, &Envelope::control(Control::Pong)).await?;
            }
            Control::SessionList => {
                wire::write_envelope(
                    &mut writer,
                    &Envelope::control(Control::SessionListResult(Vec::new())),
                )
                .await?;
            }
            Control::Drain => {
                let _ = writer.flush().await;
                eprintln!("horizon-agentd: drained, exiting");
                std::process::exit(0);
            }
            other => {
                eprintln!("horizon-agentd: {other:?} not handled yet (step 2 scope)");
            }
        }
    }
}

fn our_hello_envelope() -> Envelope {
    Envelope::control(Control::Hello(Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: BINARY_ID.to_string(),
        capabilities: Vec::new(),
    }))
}

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// `horizon-agentd` that didn't shut down cleanly), remove it and rebind.
/// If something *is* accepting, refuses to steal the path out from under a
/// live instance.
async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another horizon-agentd running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "horizon-agentd: removing stale socket {} (nothing was accepting)",
                    path.display()
                );
                std::fs::remove_file(path)?;
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(UnixListener::bind(path)?)
}

fn socket_path_from_args<I: Iterator<Item = String>>(mut args: I) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--socket=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_from_args_reads_the_space_separated_form() {
        let args = vec!["--socket".to_string(), "/tmp/sock".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock"))
        );
    }

    #[test]
    fn socket_path_from_args_reads_the_equals_form() {
        let args = vec!["--socket=/tmp/sock2".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock2"))
        );
    }

    #[test]
    fn socket_path_from_args_is_none_when_the_flag_is_absent() {
        let args: Vec<String> = vec!["--other-flag".to_string()];
        assert_eq!(socket_path_from_args(args.into_iter()), None);
    }
}
