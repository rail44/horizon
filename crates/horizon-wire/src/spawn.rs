//! Spawn-or-connect: how a client reaches a runtime daemon that may not be
//! running yet. The client half of [`crate::socket`]'s path conventions --
//! that module says *where* a daemon listens, this one gets a connected
//! socket at that path, starting the daemon once if nothing is there yet
//! (`docs/agent-runtime-split-design.md` decision 4, extended to
//! `horizon-terminald` by `docs/terminald-split-design.md` decision 1).
//!
//! Domain-free like the rest of this crate: it hands back a raw, connected
//! `UnixStream` and knows nothing about what will be spoken over it. The
//! remoc connection, and the `hello` range negotiation that rides it as the
//! first rtc call, belong to the shell-side runtime clients in
//! `src/runtime/`.
//!
//! Both daemons are the *only* place their view kind's sessions run --
//! there is no in-process fallback or daemon feature flag -- and Horizon has
//! no process-wide Tokio runtime, so `src/runtime/` gives each client a
//! dedicated current-thread runtime on a background OS thread and a slow or
//! failing daemon never blocks window startup.

use std::path::{Path, PathBuf};
use std::time::Duration;

use remoc::RemoteSend;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

use crate::codec::WireCodec;

/// Starting delay for the connect retry loop's exponential backoff
/// (doubling, capped at 1s -- see [`connect_or_spawn_daemon`]). A daemon
/// binds its socket as its first action, so a freshly spawned one accepts
/// a `connect` within milliseconds.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// The binary name `horizon-terminald` is spawned as/looked up as -- see
/// [`resolve_daemon_binary`].
const TERMINALD_BINARY_NAME: &str = "horizon-terminald";

/// [`TERMINALD_BINARY_NAME`]'s log-daemon sibling (`docs/logd-design.md`).
const LOGD_BINARY_NAME: &str = "horizon-logd";

/// Connects immediately when `horizon-terminald` is already listening;
/// otherwise starts it once and keeps retrying with capped backoff until its
/// socket is ready. The caller's runtime client owns the handshake and all
/// routing after this returns. The
/// terminal daemon is deliberately long-lived (it survives every `Reload
/// Agent Runtime`), so in practice this connects to an already-running
/// process far more often than it spawns one -- but the on-demand spawn is
/// what makes a first launch, or a recovery after `Reload Terminal Runtime`,
/// need no separate supervisor.
pub async fn connect_or_spawn_terminald_retrying(
    socket_path: &Path,
    control_socket: &Path,
) -> Result<UnixStream, String> {
    connect_or_spawn_daemon(socket_path, control_socket, TERMINALD_BINARY_NAME).await
}

/// [`connect_or_spawn_terminald_retrying`]'s `horizon-logd` twin
/// (`docs/logd-design.md`). logd has no children that need the control-plane
/// socket, so -- unlike the other two daemons -- no `control_socket` argument
/// is needed: the `HORIZON_SOCKET` env injection `daemon_command` does is
/// moot here (logd ignores it).
pub async fn connect_or_spawn_logd_retrying(socket_path: &Path) -> Result<UnixStream, String> {
    // Passing the socket path itself as the "control socket" is harmless:
    // `daemon_command` sets `HORIZON_SOCKET` to it, and logd never reads that
    // variable.
    connect_or_spawn_daemon(socket_path, socket_path, LOGD_BINARY_NAME).await
}

async fn connect_or_spawn_daemon(
    socket_path: &Path,
    control_socket: &Path,
    binary_name: &str,
) -> Result<UnixStream, String> {
    if let Ok(stream) = UnixStream::connect(socket_path).await {
        return Ok(stream);
    }
    spawn_daemon(socket_path, control_socket, binary_name)?;

    let mut delay = RETRY_DELAY;
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(delay).await,
        }
        delay = (delay * 2).min(Duration::from_secs(1));
    }
}

fn spawn_daemon(
    socket_path: &Path,
    control_socket: &Path,
    binary_name: &str,
) -> Result<(), String> {
    let binary = resolve_daemon_binary(binary_name);
    let mut command = daemon_command(&binary, socket_path, control_socket);
    detach_stdio(&mut command, socket_path);
    command.spawn().map(|_child| ()).map_err(|err| {
        format!(
            "failed to spawn {} ({err}) -- run `cargo build --workspace` to build \
             {binary_name}, then try again",
            binary.display()
        )
    })
}

/// Builds the `<daemon> --socket <path>` command [`spawn_daemon`] spawns,
/// injecting `HORIZON_SOCKET` into its environment so the daemon's own
/// children (terminald's PTY shells, and anything
/// else they shell out to) default to targeting *this* Horizon instance's
/// control socket -- `docs/cli-control-plane-design.md`'s "Discovery"
/// decision. Split out from `spawn_daemon` so the env injection is directly
/// assertable without actually spawning a process (see this module's tests).
fn daemon_command(
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

/// Where a daemon listening on `socket_path` writes its stdout and stderr:
/// beside the socket, with the extension `log` (`terminald.sock` ->
/// `terminald.log`).
fn daemon_log_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("log")
}

/// Gives the daemon stdio of its own instead of the spawner's.
///
/// A daemon outlives the Horizon that started it, and so outlives the
/// terminal that Horizon may have been started from. Inherited, its stdout
/// and stderr would be that terminal; once the terminal closes, every write
/// fails with `EIO`, and `eprintln!` panics on a failed write -- an agent
/// session then ends at its first diagnostic line. So stdin is `/dev/null`
/// and both outputs are appended to [`daemon_log_path`]: appended, so a
/// respawned daemon keeps what the previous one last wrote. A log that
/// cannot be opened sends the output nowhere rather than back to the
/// terminal.
fn detach_stdio(command: &mut std::process::Command, socket_path: &Path) {
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;

    command.stdin(Stdio::null());
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(daemon_log_path(socket_path));
    match log.and_then(|file| Ok((file.try_clone()?, file))) {
        Ok((stdout, stderr)) => {
            command.stdout(stdout).stderr(stderr);
        }
        Err(_) => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
}

/// Establishes the remoc connection over an already-connected stream and
/// takes the hub client the daemon hands over on the base channel.
///
/// Returns the client and the chmux mux task; the caller owns that task and
/// must abort it when done (which closes the socket, so a daemon's
/// one-at-a-time accept loop can serve the next connection).
///
/// This is the client-side counterpart of [`crate::daemon::serve_connection`],
/// factored here so any client (the shell's runtime clients, the board
/// library, a test harness) shares one definition.
pub async fn connect_hub_client<T: RemoteSend>(
    stream: UnixStream,
) -> Result<(T, JoinHandle<()>), String> {
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), T, WireCodec>(remoc::Cfg::default(), read_half, write_half)
            .await
            .map_err(|e| format!("remoc connect failed: {e}"))?;
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let hub = base_rx
        .recv()
        .await
        .map_err(|e| format!("base channel error: {e}"))?
        .ok_or("the daemon closed the connection before handing over its hub client")?;
    Ok((hub, conn_task))
}

/// Where to look for a daemon binary: first, an explicit override via the
/// `HORIZON_<NAME>_BINARY` env var (if set and the file exists — used by
/// tests that know the binary path via `env!("CARGO_BIN_EXE_<name>")`);
/// then right next to Horizon's own executable (the shape
/// `cargo build --workspace`/`cargo run` produces -- every workspace binary
/// lands in the same `target/debug` or `target/release` directory); then a
/// bare name resolved through `PATH` (an installed deployment, or a
/// developer who's put it there themselves). The dev-flow gotcha the
/// next-to-exe rule exists for: `cargo run` alone only rebuilds the
/// `horizon` binary, and `target/debug` is not on `PATH` by default, so a
/// bare `Command::new("horizon-terminald")` would reliably fail to find a
/// workspace build even though one exists two directories away -- see
/// [`spawn_daemon`]'s error message for the resulting actionable hint when
/// none of the locations has it.
///
/// The env-var override is `HORIZON_<NAME>_BINARY` where `<NAME>` is
/// `binary_name` with its `horizon-` prefix stripped, `-` → `_`, and
/// uppercased — e.g. `horizon-logd` → `HORIZON_LOGD_BINARY`,
/// `horizon-agent-acp` → `HORIZON_AGENT_ACP_BINARY`. Stripping the prefix avoids
/// a doubled `HORIZON_HORIZON_*` key. This lets a test pin the exact
/// binary (via `CARGO_BIN_EXE`, which cargo guarantees for the test's own
/// package) without changing production's resolution when the env var is
/// absent. Same convention shape as `$HORIZON_*_SOCKET` for socket paths.
pub fn resolve_daemon_binary(binary_name: &str) -> PathBuf {
    let stripped = binary_name.trim_prefix("horizon-");
    let env_key = format!(
        "HORIZON_{}_BINARY",
        stripped.replace('-', "_").to_uppercase()
    );
    if let Ok(path) = std::env::var(&env_key) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(binary_name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(binary_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_command_injects_the_control_socket_env_var() {
        let command = daemon_command(
            Path::new("/usr/bin/horizon-terminald"),
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

    /// What a spawned daemon writes reaches the log beside its socket, and
    /// it reads nothing from the spawner's stdin.
    #[test]
    fn a_daemon_writes_to_the_log_beside_its_socket_not_to_the_spawner() {
        let dir = std::env::temp_dir().join(format!("hzn-spawn-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("terminald.sock");
        let log = daemon_log_path(&socket);
        assert_eq!(log, dir.join("terminald.log"));
        std::fs::write(&log, "earlier daemon\n").unwrap();

        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "echo out; echo err >&2; if read line; then echo read; else echo eof; fi",
        ]);
        detach_stdio(&mut command, &socket);
        let status = command.status().unwrap();
        assert!(status.success());

        let written = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(written, "earlier daemon\nout\nerr\neof\n");
    }

    /// The daemons resolve through the same "next to the running
    /// executable, else `PATH`" rule, differing only in the file name --
    /// the property `Reload Terminal Runtime` relies on to respawn
    /// `horizon-terminald` from a `cargo build --workspace` tree.
    #[test]
    fn the_daemon_binaries_resolve_by_name_through_the_same_rule() {
        assert_eq!(
            resolve_daemon_binary("horizon-agent-acp")
                .file_name()
                .unwrap(),
            std::ffi::OsStr::new("horizon-agent-acp")
        );
        assert_eq!(
            resolve_daemon_binary(TERMINALD_BINARY_NAME)
                .file_name()
                .unwrap(),
            std::ffi::OsStr::new("horizon-terminald")
        );
    }
}
