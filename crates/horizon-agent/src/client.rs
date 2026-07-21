//! Horizon-side client for `horizon-sessiond`: spawn-or-connect (decision 4 in
//! `docs/agent-runtime-split-design.md`). Transport-agnostic on purpose --
//! this module hands back a raw, connected `UnixStream`; the v10 remoc
//! connection (and the `hello` range negotiation that rides it as the
//! first rtc call) is owned by the shared `src/sessiond` runtime.
//!
//! `horizon-sessiond` is the *only* place agent sessions run -- there is no
//! in-process fallback or daemon feature flag.
//!
//! Horizon has no process-wide Tokio runtime; `src/sessiond` owns a dedicated
//! current-thread runtime on a background OS thread so a slow or failing
//! daemon never blocks window startup.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
}
