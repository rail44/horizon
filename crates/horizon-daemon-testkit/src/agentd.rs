//! Spawning `horizon-agentd` for a test -- **hermetically**, which is the
//! whole point of this module.
//!
//! Two suites spawn this daemon: its own (`crates/horizon-agentd/tests/e2e.rs`)
//! and terminald's (`crates/horizon-terminald/tests/e2e.rs`, whose acceptance
//! test drains a real agentd to prove the terminal daemon survives it).
//! Before this module they each described the hermeticity contract in their
//! own words, and only one of them could ever be right after a change: the
//! failure mode of the stale copy is not a red test but a *green* one
//! against a less isolated daemon, up to and including reading a real
//! developer's event log. [`agentd_hermetic_command`] is now the single
//! definition both call.

use std::ffi::{OsStr, OsString};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::binary::spawn_with_link_retry;
use crate::process::{scratch_file, scratch_socket};

/// Everything one hermetically-spawned `horizon-agentd` owns on disk: the
/// socket it binds plus the two persistence paths it must be pointed away
/// from real user data at.
#[derive(Clone, Debug)]
pub struct AgentdPaths {
    pub socket_path: PathBuf,
    pub event_log_path: PathBuf,
    pub state_db_path: PathBuf,
}

impl AgentdPaths {
    /// A fresh, throwaway set -- what almost every test wants. `tag` names
    /// the suite in the scratch file names, so a leftover file after a hard
    /// failure says which suite left it.
    pub fn scratch(tag: &str) -> Self {
        Self {
            socket_path: scratch_socket(tag),
            event_log_path: scratch_file(&format!("{tag}-events"), "jsonl"),
            state_db_path: scratch_file(&format!("{tag}-state"), "duckdb"),
        }
    }

    /// Caller-chosen socket and event log with a fresh scratch DuckDB
    /// projection -- the shape the "kill it and bring a new one up on the
    /// same paths" tests need, where the log must survive but the
    /// projection is rebuilt from it anyway.
    pub fn scratch_at(tag: &str, socket_path: PathBuf, event_log_path: PathBuf) -> Self {
        Self {
            socket_path,
            event_log_path,
            state_db_path: scratch_file(&format!("{tag}-state"), "duckdb"),
        }
    }
}

/// Builds the `Command` that spawns `horizon-agentd` at `paths` --
/// **`horizon-agentd`'s hermetic-spawn contract, in one place**. Every test
/// spawn in this workspace goes through here.
///
/// The contract, and why each part of it is not optional:
///
/// * `--socket` at the caller's throwaway path, so concurrent tests never
///   meet on one socket.
/// * `HORIZON_CONFIG` pointed at a path that deliberately does not exist.
///   Without it the binary's own config loader (`main`'s
///   `horizon_config::load()`) falls back to this machine's real
///   `~/.config/horizon/config.toml`, and the startup persistence open
///   (`spawn_resume_task`/`open_persistence`) would read/rebuild from a real
///   developer's -- potentially large, potentially concurrently locked --
///   state.
/// * `HORIZON_AGENT_EVENT_LOG` at a fresh empty path, for that same reason
///   and so runs are fast and deterministic.
/// * `HORIZON_AGENT_STATE_DB` likewise. There is no "unset = disabled" state
///   for the DuckDB projection any more (`resolve_state_db_path`'s doc
///   comment): unset resolves to a real default path
///   (`$XDG_DATA_HOME/horizon/agent-state.duckdb`), which would make every
///   test process fight over the *same* real file.
/// * `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` at `/dev/null`, so the git
///   invocations agentd makes (worktree setup, session setup) see none of
///   the running user's git configuration.
///
/// The env var *names* are agentd's own, resolved inside `horizon-agent`'s
/// `config` module (`EVENT_LOG_PATH_VAR`, `STATE_DB_PATH_VAR`) and
/// `horizon-config`'s `CONFIG_PATH_VAR`; they are spelled out here because
/// those constants are crate-private. If one of them changes -- or agentd
/// grows another environment input that reaches outside the test's scratch
/// dir -- this function is the one place to update, and both suites follow.
pub fn agentd_hermetic_command(binary: &Path, paths: &AgentdPaths) -> Command {
    // Never created; its only job is to exist as a path that does not.
    let missing_config_path = scratch_file("agentd-no-such-config", "toml");
    let mut command = Command::new(binary);
    command
        .arg("--socket")
        .arg(&paths.socket_path)
        .env("HORIZON_CONFIG", &missing_config_path)
        .env("HORIZON_AGENT_EVENT_LOG", &paths.event_log_path)
        .env("HORIZON_AGENT_STATE_DB", &paths.state_db_path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    command
}

/// A recipe for one `horizon-agentd` spawn: the hermetic contract above,
/// plus whatever a single suite needs on top of it (agentd's own test-hook
/// env vars, stderr capture). Kept by the spawned [`AgentdProcess`] so it
/// can bring an identical process back up at the same paths.
#[derive(Clone, Debug)]
pub struct AgentdSpawn {
    binary: PathBuf,
    paths: AgentdPaths,
    /// Applied in order after the hermetic env; `None` means `env_remove`.
    extra_env: Vec<(OsString, Option<OsString>)>,
    capture_stderr: bool,
}

impl AgentdSpawn {
    pub fn new(binary: PathBuf, paths: AgentdPaths) -> Self {
        Self {
            binary,
            paths,
            extra_env: Vec::new(),
            capture_stderr: false,
        }
    }

    /// Sets an extra environment variable -- for a caller's own test hooks,
    /// never for the hermeticity contract itself.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.extra_env.push((
            key.as_ref().to_os_string(),
            Some(value.as_ref().to_os_string()),
        ));
        self
    }

    /// Clears an environment variable this test process may itself be
    /// running with, so a hook set for one spawn cannot leak into the next.
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.extra_env.push((key.as_ref().to_os_string(), None));
        self
    }

    /// Pipes the daemon's stderr and drains it continuously into memory, so
    /// a test can observe a line *while the process is still alive* (see
    /// [`AgentdProcess::wait_for_stderr_line`]). Without this the process
    /// inherits the test harness's stderr.
    pub fn capture_stderr(mut self) -> Self {
        self.capture_stderr = true;
        self
    }

    pub fn spawn(self) -> AgentdProcess {
        let mut command = agentd_hermetic_command(&self.binary, &self.paths);
        for (key, value) in &self.extra_env {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        if self.capture_stderr {
            command.stderr(Stdio::piped());
        }
        let mut child = spawn_with_link_retry(&mut command);

        let stderr_lines = self.capture_stderr.then(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let reader_lines = lines.clone();
            let pipe = child.stderr.take().expect("stderr should have been piped");
            thread::spawn(move || {
                let reader = std::io::BufReader::new(pipe);
                for line in reader.lines().map_while(Result::ok) {
                    reader_lines.lock().unwrap().push(line);
                }
            });
            lines
        });

        AgentdProcess {
            child,
            socket_path: self.paths.socket_path.clone(),
            event_log_path: self.paths.event_log_path.clone(),
            state_db_path: self.paths.state_db_path.clone(),
            spawn: self,
            stderr_lines,
        }
    }
}

/// Owns a spawned `horizon-agentd` child and the paths it was pointed at;
/// kills the child and removes all of them on drop so a failing assertion
/// doesn't leak either across test runs.
pub struct AgentdProcess {
    pub child: Child,
    pub socket_path: PathBuf,
    pub event_log_path: PathBuf,
    pub state_db_path: PathBuf,
    /// The recipe that produced this process, kept for
    /// [`Self::respawn_at_same_paths`].
    spawn: AgentdSpawn,
    /// Lines seen so far on this process's stderr, continuously drained by a
    /// background thread -- `Some` only when the spawn asked for
    /// [`AgentdSpawn::capture_stderr`].
    stderr_lines: Option<Arc<Mutex<Vec<String>>>>,
}

impl AgentdProcess {
    /// Polls this process's continuously drained stderr (see
    /// [`AgentdSpawn::capture_stderr`]) until a line containing `needle`
    /// appears, or panics after a generous timeout. Panics immediately if
    /// this process wasn't spawned with stderr capture enabled.
    pub async fn wait_for_stderr_line(&self, needle: &str) -> String {
        let lines = self
            .stderr_lines
            .as_ref()
            .expect("stderr capture must be enabled via AgentdSpawn::capture_stderr");
        for _ in 0..500 {
            if let Some(line) = lines
                .lock()
                .unwrap()
                .iter()
                .find(|line| line.contains(needle))
                .cloned()
            {
                return line;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("gave up waiting for a stderr line containing {needle:?}");
    }

    /// Kills this process with `SIGKILL` (`Child::kill` sends `SIGKILL` on
    /// Unix -- no graceful shutdown, no chance to flush or unlink the
    /// socket) and waits for it to actually exit, so a caller that then
    /// spawns a fresh process at the same paths isn't racing the old one for
    /// the socket. The paths are left on disk deliberately; see
    /// [`Self::leak_keeping_paths`].
    pub fn kill_and_wait(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.leak_keeping_paths();
    }

    /// Brings an identical process back up at exactly the same socket, event
    /// log and DuckDB projection -- what `Reload Agent Runtime` does once
    /// the drain has completed and the old process is gone.
    pub fn respawn_at_same_paths(self) -> Self {
        let spawn = self.spawn.clone();
        self.leak_keeping_paths();
        spawn.spawn()
    }

    /// Skips `Drop` (which removes the socket, event log and DuckDB
    /// projection) so a successor process can be brought up on exactly those
    /// paths: the point of a respawn test is that the *log* is still there.
    /// `std::mem::forget` is the only way to opt out of a `Drop` impl, and
    /// leaking a `Child` handle for an already-reaped or about-to-be-replaced
    /// process costs nothing a test cares about.
    fn leak_keeping_paths(self) {
        std::mem::forget(self);
    }
}

impl Drop for AgentdProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.event_log_path);
        let _ = std::fs::remove_file(&self.state_db_path);
    }
}
