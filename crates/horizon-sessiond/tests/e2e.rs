//! End-to-end test against the real `horizon-sessiond` binary (spawned via
//! [`resolve_sessiond_binary`], which reads `CARGO_BIN_EXE_horizon-sessiond`
//! as a runtime environment variable rather than the same-named compile-time
//! `env!()` bake -- only available because this test lives in the same
//! package as the `[[bin]]` target -- see `docs/tasks/backlog.md` #40) --
//! see `docs/agent-runtime-split-design.md`'s step 2 deliverables.
//!
//! Since the v10 remoc cutover (`docs/remoc-adoption-design.md`) these talk
//! to the daemon over the actual `SessionHub` rtc trait on the actual unix
//! socket, through the [`HubTestClient`] harness below: `hello` range
//! negotiation, the terminal/agent attach calls returning channel-bearing
//! attachments, and `drain`. The frame-delivery semantics are unchanged in
//! v10 (`Snapshot`/`FrameDiff` still travel verbatim on the attachment's
//! updates channel), so the terminal tests still assert snapshot-then-diff.
//! Cross-generation recovery (a v10 UI meeting a JSONL daemon) is covered
//! on the *client* side, in `src/sessiond/tests.rs`, where the runtime that
//! owns the probe-drain-respawn sequence lives.
//!
//! The tests use a multi-thread runtime because the remoc chmux mux task
//! must be polled concurrently with the test's own awaits (adoption
//! condition 3) while some helpers block briefly (process spawn/kill,
//! stderr reads); a current-thread runtime would starve the mux.

use std::io::{BufRead, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use horizon_agent::contract::{
    Command as AgentCommand, Event, Exit, MessageRole, ProviderEvent, ProviderId, SessionId,
    SessionState, TurnEndReason,
};
use horizon_agent::frame::agent_frame_from_events;
use horizon_agent::persistence::event_log::{Appender, WriterHandle, WriterInit};
use horizon_agent::roles::RoleId;
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use horizon_session_protocol::{
    CappedReceiver, ClientHello, HubError, SessionHub as _, SessionHubClient, VersionRange,
    WireCodec, CONTROL_MAX_ITEM_BYTES, FRAME_MAX_ITEM_BYTES, MIN_SUPPORTED_PROTOCOL_VERSION,
    SESSION_PROTOCOL_VERSION, TOOL_IO_MAX_ITEM_BYTES,
};
use horizon_terminal_core::{
    apply_frame_diff, TerminalColorScheme, TerminalCommand, TerminalFrame, TerminalSize,
    TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use remoc::rch;
use tokio::net::UnixStream;

/// The env var `horizon-sessiond`'s `main` reads to artificially delay its
/// event-log-read-plus-resume phase -- see that binary's own doc comment on
/// the constant of the same name. Test-only; never set outside this file.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_SESSIOND_TEST_RESUME_DELAY_MS";

/// The env var `horizon-sessiond`'s `main` reads to artificially delay its
/// background DuckDB rebuild task -- the DuckDB analogue of
/// [`TEST_RESUME_DELAY_MS_VAR`], letting a test prove `hello`/`session_list`
/// stay reachable while a slow rebuild is still running. Test-only; never
/// set outside this file.
const TEST_DUCKDB_REBUILD_DELAY_MS_VAR: &str = "HORIZON_SESSIOND_TEST_DUCKDB_REBUILD_DELAY_MS";

/// How long to wait once before re-probing/re-spawning after finding the
/// `horizon-sessiond` binary transiently missing -- see
/// [`resolve_sessiond_binary`] and [`spawn_sessiond`]. A single bounded
/// wait, not a polling loop: the race this covers is cargo's own
/// artifact-uplift `remove_file`-then-relink (a link syscall's worth of
/// time), confirmed locally (backlog #36) to close well under this.
const TRANSIENT_LINK_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Name of the runtime-set env var this resolver reads first -- same name
/// as the compile-time `env!()` bake [`resolve_sessiond_binary`] falls back
/// to, but a *different* mechanism: see that function's doc comment.
const CARGO_BIN_EXE_VAR: &str = "CARGO_BIN_EXE_horizon-sessiond";

/// Resolves the `horizon-sessiond` binary to spawn, preferring
/// `std::env::var(CARGO_BIN_EXE_VAR)` -- a genuine OS environment variable
/// of *this test process*, re-injected fresh by cargo/cargo-nextest on
/// every invocation -- over the same-named `env!("CARGO_BIN_EXE_
/// horizon-sessiond")` compile-time bake, which is a constant frozen into
/// this test binary's own compiled code at the moment it was built.
///
/// Cargo documents setting `CARGO_BIN_EXE_<name>` twice, for two different
/// consumers: once as a `rustc` build-time env var (readable only via
/// `env!()`/`option_env!()`, baked into the compiled artifact), and again
/// as the literal runtime environment of the spawned test *process* itself
/// every time `cargo test`/`cargo nextest run` executes it -- see
/// <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
/// ("Cargo sets several environment variables when tests are run. You can
/// retrieve the values when the tests are run"). Confirmed empirically
/// against this repo's actual toolchain (both `cargo test` and `cargo
/// nextest run`) while diagnosing backlog #40.
///
/// That distinction is load-bearing here because this repo's
/// `build.build-dir` split (`AGENTS.md` "Build setup") makes test binaries
/// themselves shared, reusable *intermediate* build artifacts: unlike
/// `horizon-sessiond` (a real `[[bin]]` target, uplifted fresh into every
/// worktree's own `target/`), a compiled `e2e` test binary can be reused
/// unchanged (relinked, not recompiled) into a fresh worktree without ever
/// re-running `rustc` -- confirmed by inspecting the shared build-dir
/// directly: `deps/e2e-*` binaries live only under `{cargo-cache-home}/
/// horizon-build-dir/`, never uplifted into any worktree's `target/`, so
/// `std::env::current_exe()` for this test binary always resolves *inside
/// the shared build-dir*, not this worktree -- it cannot anchor a
/// per-worktree path at all. A stale, reused test binary's `env!()` bake
/// therefore still holds the absolute path from whichever worktree
/// compiled it *first*, and once that worktree is deleted (the normal
/// worker lifecycle), that path is permanently dead. The runtime env var
/// has no such problem: cargo/nextest compute and inject it fresh for
/// *this* invocation, from *this* worktree's own `cargo metadata`,
/// regardless of how stale the test binary's compiled code is.
///
/// The `env!()` bake is kept only as a defensive fallback, for the (today
/// unobserved) case of this test binary being invoked directly, bypassing
/// `cargo test`/`cargo nextest run`'s own env injection. Either path can
/// still be transiently missing due to cargo's own non-atomic
/// artifact-uplift step (`docs/tasks/backlog.md` #36); that race is
/// handled at the `spawn()` call site by [`spawn_sessiond`], not here.
fn resolve_sessiond_binary() -> PathBuf {
    if let Ok(runtime_var) = std::env::var(CARGO_BIN_EXE_VAR) {
        let path = PathBuf::from(runtime_var);
        if path.is_file() {
            return path;
        }
    }

    let baked_in = PathBuf::from(env!("CARGO_BIN_EXE_horizon-sessiond"));
    if baked_in.is_file() {
        return baked_in;
    }

    panic!(
        "could not locate the horizon-sessiond binary to spawn for this e2e test -- probed \
         runtime env var {CARGO_BIN_EXE_VAR} = {:?} and compile-time \
         CARGO_BIN_EXE_horizon-sessiond bake = {} (exists = {}) -- see docs/tasks/backlog.md #40",
        std::env::var(CARGO_BIN_EXE_VAR),
        baked_in.display(),
        baked_in.is_file(),
    );
}

/// Proves the runtime resolution [`resolve_sessiond_binary`] prefers
/// actually finds an existing binary, and that it's the same binary the
/// compile-time `env!()` bake would have named -- the mechanism backlog
/// #40's fix relies on: both are cargo's own idea of "the `horizon-sessiond`
/// binary for this test run", differing only in *when* the value is
/// computed (build time vs. this exact invocation), not *what* it points
/// at, for a normal (non-stale-cache) run like this one.
#[test]
fn resolve_sessiond_binary_finds_an_existing_binary_via_the_runtime_env_var() {
    let runtime_var = std::env::var(CARGO_BIN_EXE_VAR)
        .expect("cargo/cargo-nextest must set CARGO_BIN_EXE_horizon-sessiond at test runtime");
    let runtime_path = PathBuf::from(&runtime_var);
    assert!(
        runtime_path.is_file(),
        "runtime env var {CARGO_BIN_EXE_VAR} = {runtime_var} does not point at an existing file"
    );

    let resolved = resolve_sessiond_binary();
    assert_eq!(
        resolved, runtime_path,
        "resolve_sessiond_binary must prefer the runtime env var over the compile-time bake"
    );

    // Deliberately NOT asserted: `resolved == env!("CARGO_BIN_EXE_...")`.
    // Divergence between the runtime var and the compile-time bake is this
    // fix's NORMAL operating mode under the shared build-dir -- it happens
    // whenever the cached test binary was compiled in a sibling worktree
    // (possibly since deleted). An equality assertion here failed the
    // integration gate the very first time that scenario occurred; see
    // `docs/tasks/backlog.md` #40.
}

fn spawn_sessiond(command: &mut Command) -> Child {
    match command.spawn() {
        Ok(child) => child,
        Err(first_error) if first_error.kind() == ErrorKind::NotFound => {
            thread::sleep(TRANSIENT_LINK_RETRY_DELAY);
            command.spawn().unwrap_or_else(|retry_error| {
                let program = command.get_program().to_owned();
                panic!(
                    "failed to spawn horizon-sessiond even after a retry for a transient link \
                     window: first error = {first_error}, retry error = {retry_error}, program \
                     = {} (exists = {}) -- see docs/tasks/backlog.md #36",
                    program.to_string_lossy(),
                    Path::new(&program).is_file(),
                )
            })
        }
        Err(error) => panic!("failed to spawn horizon-sessiond: {error}"),
    }
}

/// Owns the spawned `horizon-sessiond` child and its socket path; kills the
/// child and removes the socket file on drop so a failing assertion doesn't
/// leak either across test runs.
struct SessiondProcess {
    child: Child,
    socket_path: PathBuf,
    event_log_path: PathBuf,
    state_db_path: PathBuf,
    /// Lines seen so far on this process's stderr, continuously drained by
    /// a background thread -- `Some` only for a process spawned via
    /// [`Self::spawn_at_with_duckdb_options`], which is the only
    /// constructor that pipes (rather than inherits) stderr. Needed by
    /// [`Self::wait_for_stderr_line`] to observe a spawn's own background
    /// DuckDB rebuild-or-skip decision (task 2) *while the process is still
    /// alive* -- there is no over-the-wire signal for it (task 1's whole
    /// point is that nothing waits on it), so a test must poll stderr
    /// before killing the process, not just read it all after the fact.
    stderr_lines: Option<Arc<Mutex<Vec<String>>>>,
}

impl SessiondProcess {
    /// Spawns `horizon-sessiond` pointed at a throwaway event log path and a
    /// nonexistent config file -- **hermetic on purpose**: without this,
    /// the binary's own config loader (`main`'s `horizon_config::load()`
    /// call) falls back to this machine's real
    /// `~/.config/horizon/config.toml`, and step 3's startup persistence
    /// open (`spawn_resume_task`/`open_persistence` in `main.rs`) would then
    /// read/rebuild-from a real developer's (potentially large, potentially
    /// concurrently-locked) event log and DuckDB file. Every test gets its
    /// own fresh, empty log path so runs are fast, deterministic, and never
    /// touch real user data.
    fn spawn() -> Self {
        // Keep the socket path well under SUN_LEN (~104 bytes on macOS):
        // temp_dir() alone is ~50 bytes, so a long descriptive file name
        // pushes bind() into "path must be shorter than SUN_LEN". The
        // event log is a regular file and free of that limit.
        let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let socket_path = std::env::temp_dir().join(format!("hzn-e2e-{short_id}.sock"));
        let event_log_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-events-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        Self::spawn_at(socket_path, event_log_path)
    }

    /// Same as [`Self::spawn`], but pointed at caller-chosen paths -- the
    /// seam [`Self::respawn_at_same_paths`] (step 4's "kill -9 mid-session,
    /// respawn" tests) uses to bring up a *second* process against the
    /// *first* process's own socket/event-log paths, simulating a real
    /// restart.
    fn spawn_at(socket_path: PathBuf, event_log_path: PathBuf) -> Self {
        Self::spawn_at_with_resume_delay(socket_path, event_log_path, None)
    }

    /// Same as [`Self::spawn_at`], but additionally sets `horizon-sessiond`'s
    /// test-only [`TEST_RESUME_DELAY_MS_VAR`] hook when `resume_delay_ms` is
    /// `Some` -- for the bind-first ordering test, which needs the
    /// log-read-plus-resume phase to take long enough that hello answering
    /// before it finishes (and `session_list` waiting for it) is provably a
    /// consequence of the ordering fix, not incidental timing.
    fn spawn_at_with_resume_delay(
        socket_path: PathBuf,
        event_log_path: PathBuf,
        resume_delay_ms: Option<u64>,
    ) -> Self {
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        // The DuckDB projection has no "unset = disabled" state any more
        // (`resolve_state_db_path`'s doc comment) -- an unset
        // `HORIZON_AGENT_STATE_DB` now resolves to a real default path
        // (`$XDG_DATA_HOME/horizon/agent-state.duckdb`), which would make
        // every test process fight over the *same* real file instead of
        // each other's throwaway `event_log_path`. Point it at its own
        // fresh temp path for the same hermeticity reason `event_log_path`
        // already gets one.
        let state_db_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-state-{}-{}.duckdb",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(resolve_sessiond_binary());
        command
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path);
        match resume_delay_ms {
            Some(delay_ms) => {
                command.env(TEST_RESUME_DELAY_MS_VAR, delay_ms.to_string());
            }
            None => {
                command.env_remove(TEST_RESUME_DELAY_MS_VAR);
            }
        }
        let child = spawn_sessiond(&mut command);
        Self {
            child,
            socket_path,
            event_log_path,
            state_db_path,
            stderr_lines: None,
        }
    }

    /// Same as [`Self::spawn_at`], but additionally sets `horizon-sessiond`'s
    /// test-only [`TEST_DUCKDB_REBUILD_DELAY_MS_VAR`] hook -- for proving
    /// the DuckDB rebuild (task 1 of the readiness fix) no longer sits on
    /// the resume-readiness path `hello`/`session_list` block on.
    fn spawn_at_with_duckdb_rebuild_delay(
        socket_path: PathBuf,
        event_log_path: PathBuf,
        rebuild_delay_ms: u64,
    ) -> Self {
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let state_db_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-state-{}-{}.duckdb",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(resolve_sessiond_binary());
        command
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .env_remove(TEST_RESUME_DELAY_MS_VAR)
            .env(
                TEST_DUCKDB_REBUILD_DELAY_MS_VAR,
                rebuild_delay_ms.to_string(),
            );
        let child = spawn_sessiond(&mut command);
        Self {
            child,
            socket_path,
            event_log_path,
            state_db_path,
            stderr_lines: None,
        }
    }

    /// Same as [`Self::spawn_at`], but with an explicit `state_db_path`
    /// (rather than the fresh random one every other constructor picks) and
    /// piped, continuously drained stderr (see [`Self::wait_for_stderr_line`])
    /// -- both needed only by task 2's skip/rebuild tests below: they must
    /// point two successive spawns at the *same* DuckDB file to prove the
    /// second one either skips or redoes the rebuild, and must observe that
    /// spawn's own rebuild-or-skip decision before killing the process.
    fn spawn_at_with_duckdb_options(
        socket_path: PathBuf,
        event_log_path: PathBuf,
        state_db_path: PathBuf,
    ) -> Self {
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-sessiond-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(resolve_sessiond_binary());
        command
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .env_remove(TEST_RESUME_DELAY_MS_VAR)
            .env_remove(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
            .stderr(Stdio::piped());
        let mut child = spawn_sessiond(&mut command);

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let reader_lines = stderr_lines.clone();
        let pipe = child.stderr.take().expect("stderr should have been piped");
        thread::spawn(move || {
            let reader = std::io::BufReader::new(pipe);
            for line in reader.lines().map_while(Result::ok) {
                reader_lines.lock().unwrap().push(line);
            }
        });

        Self {
            child,
            socket_path,
            event_log_path,
            state_db_path,
            stderr_lines: Some(stderr_lines),
        }
    }

    /// Polls this process's continuously drained stderr (see [`Self::
    /// spawn_at_with_duckdb_options`]) until a line containing `needle`
    /// appears, or panics after a generous timeout. Panics immediately if
    /// this process wasn't spawned with stderr capture enabled.
    async fn wait_for_stderr_line(&self, needle: &str) -> String {
        let lines = self
            .stderr_lines
            .as_ref()
            .expect("stderr capture must be enabled via spawn_at_with_duckdb_options");
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
    /// spawns a fresh process at the same paths (`Self::spawn_at`) isn't
    /// racing the old one for the socket.
    fn kill_and_wait(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Consumed by value and left to leak its paths on disk deliberately
        // (unlike `Drop`, which removes them) -- the caller is about to
        // spawn a fresh process at these same paths and needs the event log
        // file to still be there; `std::mem::forget` skips `Drop` entirely.
        std::mem::forget(self);
    }
}

impl Drop for SessiondProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.event_log_path);
        let _ = std::fs::remove_file(&self.state_db_path);
    }
}

async fn connect_with_retry(path: &std::path::Path) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "horizon-sessiond never accepted a connection on {}",
        path.display()
    );
}

async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    for _ in 0..200 {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("horizon-sessiond did not exit in time");
}

// --- the remoc hub test harness --------------------------------------------

/// A connected `SessionHub` client over the real socket: the v10 successor
/// of the JSONL `connect_and_handshake` split halves. Owns the chmux mux
/// task (aborted on drop, which closes the socket so the daemon's
/// one-at-a-time accept loop can serve the next connection) and holds the
/// connection-global `HubHello` channels.
struct HubTestClient {
    hub: SessionHubClient<WireCodec>,
    negotiated: u32,
    binary_id: String,
    host_tools: CappedReceiver<HostToolRequest, TOOL_IO_MAX_ITEM_BYTES>,
    host_tool_responses: rch::mpsc::Sender<HostToolResponse, WireCodec>,
    skipped_lines: CappedReceiver<String, CONTROL_MAX_ITEM_BYTES>,
    conn_task: tokio::task::JoinHandle<()>,
}

impl Drop for HubTestClient {
    fn drop(&mut self) {
        self.conn_task.abort();
    }
}

/// Establishes the remoc connection over an already-connected stream, hands
/// the daemon its client, and runs `hello` with the given advertised range.
async fn establish_hub(
    stream: UnixStream,
    supported: VersionRange,
) -> Result<HubTestClient, HubError> {
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        )
        .await
        .expect("remoc connect to the real daemon");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let hub = base_rx
        .recv()
        .await
        .expect("base channel recv")
        .expect("the daemon should hand over a hub client");

    let client_hello = ClientHello {
        supported,
        binary_id: "test-client".to_string(),
    };
    match hub.hello(client_hello).await {
        Ok(hello) => Ok(HubTestClient {
            hub,
            negotiated: hello.negotiated,
            binary_id: hello.binary_id,
            host_tools: hello.host_tools,
            host_tool_responses: hello.host_tool_responses,
            skipped_lines: hello.skipped_lines,
            conn_task,
        }),
        Err(error) => {
            // The connection stays alive (the mux task keeps running) so a
            // rejected client can still call `drain` -- return a live hub
            // for that, threaded through the error's own path in the one
            // test that needs it.
            conn_task.abort();
            Err(error)
        }
    }
}

/// Connects to the real socket and completes `hello` at this build's own
/// advertised range -- every session-hosting test's entry point.
async fn connect_hub(socket_path: &Path) -> HubTestClient {
    let stream = connect_with_retry(socket_path).await;
    establish_hub(stream, VersionRange::ours())
        .await
        .expect("hello should succeed at a matching version range")
}

impl HubTestClient {
    /// Gracefully drains the daemon. The daemon exits inside the call, so
    /// the reply never travels -- the call resolves as a transport error,
    /// which is expected; the caller confirms the exit via
    /// [`wait_for_exit`].
    async fn drain(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.hub.drain()).await;
    }
}

const TERMINAL_UPDATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Reads the next update from an attachment's channel, or panics on
/// timeout/disconnect.
async fn read_terminal_update(
    updates: &mut CappedReceiver<TerminalUpdate, FRAME_MAX_ITEM_BYTES>,
) -> TerminalUpdate {
    tokio::time::timeout(TERMINAL_UPDATE_TIMEOUT, updates.recv())
        .await
        .expect("timed out waiting for a terminal update")
        .expect("terminal update channel error")
        .expect("the daemon should keep the terminal attachment open")
}

async fn send_terminal_command(
    commands: &rch::mpsc::Sender<TerminalCommand, WireCodec>,
    command: TerminalCommand,
) {
    commands
        .send(command)
        .await
        .expect("send a terminal command");
}

fn terminal_spec(
    fallback_cwd: PathBuf,
    spawn_source_session_id: Option<uuid::Uuid>,
) -> TerminalSpawnSpec {
    TerminalSpawnSpec {
        shell: "/bin/sh".into(),
        args: vec!["-i".into()],
        term: "xterm-256color".into(),
        scrollback_lines: 1_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: "/tmp/horizon-sessiond-e2e-control.sock".into(),
        fallback_cwd,
        spawn_source_session_id,
        initial_size: TerminalSize::new(80, 24),
    }
}

/// Folds updates from an attachment's channel into `frame` until its text
/// contains `needle`; returns the accumulated frame and whether any diff
/// (not just snapshots) was observed along the way.
async fn collect_terminal_frame_until(
    updates: &mut CappedReceiver<TerminalUpdate, FRAME_MAX_ITEM_BYTES>,
    mut frame: TerminalFrame,
    needle: &str,
) -> (TerminalFrame, bool) {
    let mut saw_diff = false;
    for _ in 0..100 {
        match read_terminal_update(updates).await {
            TerminalUpdate::Snapshot(snapshot) => frame = snapshot,
            TerminalUpdate::FrameDiff(diff) => {
                saw_diff = true;
                frame = apply_frame_diff(&frame, &diff);
            }
            TerminalUpdate::Error(error) => {
                panic!("terminal error while waiting for {needle:?}: {error}")
            }
            TerminalUpdate::Exited => panic!("terminal exited while waiting for {needle:?}"),
            TerminalUpdate::Title(_)
            | TerminalUpdate::Bell
            | TerminalUpdate::Clipboard { .. }
            | TerminalUpdate::Unknown => {}
        }
        if frame.text().contains(needle) {
            return (frame, saw_diff);
        }
    }
    panic!(
        "gave up waiting for {needle:?}; last frame: {:?}",
        frame.text()
    );
}

/// Reads events from an agent attachment's channel until `predicate`
/// matches one, returning every event observed (including the matching
/// one), in arrival order. Skips the non-`Event` announcements
/// (`ToolCallProgress`, `SessionModel`, `WorkspaceRootResolved`) that share
/// the channel. Panics after a generous number of reads.
async fn collect_events_until(
    events: &mut CappedReceiver<AgentWireEvent, TOOL_IO_MAX_ITEM_BYTES>,
    mut predicate: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let mut collected = Vec::new();
    for _ in 0..400 {
        let wire_event = tokio::time::timeout(Duration::from_secs(120), events.recv())
            .await
            .expect("timed out waiting for an agent event")
            .expect("agent event channel error")
            .expect("the daemon should keep streaming events, not close the attachment");
        if let AgentWireEvent::Event(event) = wire_event {
            let done = predicate(&event);
            collected.push(event);
            if done {
                return collected;
            }
        }
    }
    panic!("gave up waiting for the expected event after 400 reads; got: {collected:?}");
}

/// Reads a session's replayed events off a fresh `attach_agent` attachment
/// (`AgentAttachment::events`) until they go quiet -- `attach_agent`'s
/// replay burst has no single terminal event to watch for. Same two-wait
/// shape as the JSONL era: a long first-event budget (the daemon's
/// `replay_events` can take real time under contention), then a short
/// quiescence window once the burst starts.
async fn collect_replayed_events(
    events: &mut CappedReceiver<AgentWireEvent, TOOL_IO_MAX_ITEM_BYTES>,
) -> Vec<Event> {
    const REPLAY_FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(120);
    const REPLAY_QUIESCENCE_WINDOW: Duration = Duration::from_millis(500);

    let mut collected = Vec::new();
    let mut budget = REPLAY_FIRST_EVENT_TIMEOUT;
    loop {
        match tokio::time::timeout(budget, events.recv()).await {
            Ok(Ok(Some(AgentWireEvent::Event(event)))) => {
                collected.push(event);
                budget = REPLAY_QUIESCENCE_WINDOW;
            }
            // Non-event announcements (SessionModel, etc.) also arrive on
            // this channel -- keep waiting, they don't end the burst.
            Ok(Ok(Some(_))) => budget = REPLAY_QUIESCENCE_WINDOW,
            Ok(Ok(None)) => panic!("attachment closed while collecting replayed events"),
            Ok(Err(err)) => panic!("channel error while collecting replayed events: {err}"),
            Err(_timeout) => return collected,
        }
    }
}

/// Reads the connection-global host-tool request channel
/// (`HubHello::host_tools`) until a request arrives.
async fn read_host_tool_request(client: &mut HubTestClient) -> HostToolRequest {
    tokio::time::timeout(Duration::from_secs(120), client.host_tools.recv())
        .await
        .expect("timed out waiting for a host-tool request")
        .expect("host-tool channel error")
        .expect("the daemon should keep the host-tool channel open")
}

async fn respond_host_tool(client: &HubTestClient, response: HostToolResponse) {
    client
        .host_tool_responses
        .send(response)
        .await
        .expect("send a host-tool response");
}

fn mock_provider_id() -> ProviderId {
    ProviderId("builtin.agent.mock".to_string())
}

fn session_new(session_id: SessionId) -> SessionNew {
    SessionNew {
        session_id,
        provider_id: mock_provider_id(),
        role_id: None,
        workspace_root: None,
        spawn_source_session_id: None,
        isolate: false,
    }
}

fn session_new_with_role(session_id: SessionId, role_id: RoleId) -> SessionNew {
    SessionNew {
        session_id,
        provider_id: mock_provider_id(),
        role_id: Some(role_id),
        workspace_root: None,
        spawn_source_session_id: None,
        isolate: false,
    }
}

/// Writes a fixture event log directly at `path`, one session per
/// `(SessionId, Vec<Event>)` pair, via the same `WriterHandle`/`Appender`
/// machinery `horizon-agent`'s own event-log tests use -- for tests below
/// that need a specific pre-existing log *before* `horizon-sessiond` itself
/// ever runs. Every record gets [`mock_provider_id`] as its provider id.
fn write_session_fixture(path: &std::path::Path, sessions: Vec<(SessionId, Vec<Event>)>) {
    let (writer, init_rx) = WriterHandle::open(path);
    match init_rx
        .recv()
        .expect("fixture writer should report a startup outcome")
    {
        WriterInit::Ready(_) => {}
        WriterInit::Failed(error) => {
            panic!("fixture writer failed to open {}: {error}", path.display())
        }
    }
    for (session_id, events) in sessions {
        let mut appender =
            Appender::new(writer.clone(), session_id, Some(mock_provider_id()), None);
        appender
            .append_provider_events(events.into_iter().map(ProviderEvent::from).collect())
            .expect("append fixture events");
    }
    writer.flush().expect("flush fixture events");
}

/// Polls `path`'s on-disk event log until a record for `session_id`
/// matching `predicate` appears, or panics after a generous timeout. See
/// the JSONL-era note preserved on `killed_sessiond...`: a client can
/// observe an event over the wire before it is durable, so kill-based tests
/// must wait for the disk write.
async fn wait_for_persisted_event(
    path: &std::path::Path,
    session_id: SessionId,
    mut predicate: impl FnMut(&Event) -> bool,
) {
    for _ in 0..200 {
        if let Ok(report) = horizon_agent::persistence::event_log::read(path) {
            if report
                .records
                .iter()
                .any(|record| record.session_id == session_id && predicate(&record.event))
            {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "gave up waiting for the expected event to reach disk at {}",
        path.display()
    );
}

// --- tests -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_negotiates_lists_agents_and_drains_over_the_real_socket() {
    let mut sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    // hello's range negotiation settles on this build's version, and the
    // reply carries the daemon's binary id.
    assert_eq!(client.negotiated, SESSION_PROTOCOL_VERSION);
    assert_eq!(
        client.binary_id,
        concat!("horizon-sessiond/", env!("CARGO_PKG_VERSION"))
    );

    // No sessions yet.
    assert_eq!(client.hub.list_agents().await.unwrap(), Vec::new());

    client.drain().await;
    let status = wait_for_exit(&mut sessiond.child).await;
    assert!(
        status.success(),
        "horizon-sessiond should exit 0 after drain, got {status:?}"
    );
}

/// The v10 successor of the JSONL cross-version rejection tests: a remoc
/// client whose advertised range does not overlap the daemon's
/// (`[MIN_SUPPORTED, current]`) is rejected by `hello` with an explicit
/// `IncompatibleVersion` error naming both ranges -- and the connection
/// stays alive enough for the one thing a rejected client may still do:
/// `drain`, so the auto-recovery path can restart the daemon at a
/// compatible version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_version_range_is_rejected_but_drain_still_works() {
    let mut sessiond = SessiondProcess::spawn();

    // A client that only speaks a future version the daemon doesn't.
    let future = SESSION_PROTOCOL_VERSION + 5;
    let stream = connect_with_retry(&sessiond.socket_path).await;
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        )
        .await
        .unwrap();
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let hub = base_rx.recv().await.unwrap().unwrap();

    let result = hub
        .hello(ClientHello {
            supported: VersionRange {
                min_supported: future,
                current: future,
            },
            binary_id: "future-horizon".to_string(),
        })
        .await;
    match result {
        Err(HubError::IncompatibleVersion { client, daemon }) => {
            assert_eq!(client.current, future);
            assert_eq!(daemon.min_supported, MIN_SUPPORTED_PROTOCOL_VERSION);
            assert_eq!(daemon.current, SESSION_PROTOCOL_VERSION);
        }
        Err(other) => panic!("expected IncompatibleVersion, got {other:?}"),
        Ok(_) => panic!("a disjoint version range must be rejected"),
    }

    // The version-stable `drain` still works on the same connection.
    let _ = tokio::time::timeout(Duration::from_secs(5), hub.drain()).await;
    let status = wait_for_exit(&mut sessiond.child).await;
    assert!(
        status.success(),
        "horizon-sessiond should exit 0 after a post-rejection drain, got {status:?}"
    );
    conn_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_create_diff_reconnect_attach_and_shutdown_over_the_real_socket() {
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let client = connect_hub(&sessiond.socket_path).await;

    let mut attachment = client
        .hub
        .create_terminal(session_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .expect("create should succeed");
    let TerminalUpdate::Snapshot(initial) = read_terminal_update(&mut attachment.updates).await
    else {
        panic!("terminal create must begin with a full snapshot");
    };

    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'HORIZON_DIFF_MARKER\\n'\n".to_vec()),
    )
    .await;
    let (frame, saw_diff) =
        collect_terminal_frame_until(&mut attachment.updates, initial, "HORIZON_DIFF_MARKER").await;
    assert!(
        saw_diff,
        "updates after the create baseline should be diffs"
    );

    // Disconnect this client entirely; the terminal session keeps running
    // (process-scoped), so a fresh connection can reattach.
    drop(attachment);
    drop(client);

    let client = connect_hub(&sessiond.socket_path).await;
    let mut attachment = client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("attach on a fresh connection should succeed");
    let TerminalUpdate::Snapshot(attached) = read_terminal_update(&mut attachment.updates).await
    else {
        panic!("attach on a new connection must reset to a full snapshot");
    };
    assert!(attached.text().contains("HORIZON_DIFF_MARKER"));
    assert!(frame.text().contains("HORIZON_DIFF_MARKER"));

    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'HORIZON_REATTACH_MARKER\\n'\n".to_vec()),
    )
    .await;
    let (_, saw_diff) =
        collect_terminal_frame_until(&mut attachment.updates, attached, "HORIZON_REATTACH_MARKER")
            .await;
    assert!(saw_diff, "reattached PTY should continue streaming diffs");

    send_terminal_command(&attachment.commands, TerminalCommand::Shutdown).await;
    for _ in 0..20 {
        if matches!(
            read_terminal_update(&mut attachment.updates).await,
            TerminalUpdate::Exited
        ) {
            return;
        }
    }
    panic!("terminal shutdown did not produce an exited update");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_list_is_sorted_and_a_missing_attach_is_explicit() {
    let sessiond = SessiondProcess::spawn();
    let high_id = uuid::Uuid::from_u128(2);
    let low_id = uuid::Uuid::from_u128(1);

    let client = connect_hub(&sessiond.socket_path).await;
    assert_eq!(client.hub.list_terminals().await.unwrap(), Vec::new());

    // Create two terminals across two connections; both survive the
    // disconnect (process-scoped sessions).
    let mut high = client
        .hub
        .create_terminal(high_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    assert!(matches!(
        read_terminal_update(&mut high.updates).await,
        TerminalUpdate::Snapshot(_)
    ));
    drop(high);
    drop(client);

    let client = connect_hub(&sessiond.socket_path).await;
    let mut low = client
        .hub
        .create_terminal(low_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    assert!(matches!(
        read_terminal_update(&mut low.updates).await,
        TerminalUpdate::Snapshot(_)
    ));
    drop(low);
    drop(client);

    let client = connect_hub(&sessiond.socket_path).await;
    assert_eq!(
        client.hub.list_terminals().await.unwrap(),
        vec![
            TerminalSummary { session_id: low_id },
            TerminalSummary {
                session_id: high_id
            },
        ]
    );

    let missing_id = uuid::Uuid::from_u128(3);
    assert!(matches!(
        client.hub.attach_terminal(missing_id).await,
        Err(HubError::TerminalNotFound)
    ));

    let low = client.hub.attach_terminal(low_id).await.unwrap();
    let high = client.hub.attach_terminal(high_id).await.unwrap();
    send_terminal_command(&low.commands, TerminalCommand::Shutdown).await;
    send_terminal_command(&high.commands, TerminalCommand::Shutdown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_spawn_uses_fallback_and_source_session_cwds() {
    let sessiond = SessiondProcess::spawn();
    let root = std::env::temp_dir().join(format!("hzn-cwd-e2e-{}", uuid::Uuid::new_v4()));
    let source_cwd = root.join("source");
    let fallback_cwd = root.join("fallback");
    std::fs::create_dir_all(&source_cwd).unwrap();
    std::fs::create_dir_all(&fallback_cwd).unwrap();
    let source_cwd = source_cwd.canonicalize().unwrap();
    let wide = TerminalSize::new(200, 24);

    let source_id = uuid::Uuid::new_v4();
    let target_id = uuid::Uuid::new_v4();
    let client = connect_hub(&sessiond.socket_path).await;

    let mut source_spec = terminal_spec(source_cwd.clone(), None);
    source_spec.initial_size = wide;
    let mut source = client
        .hub
        .create_terminal(source_id, source_spec)
        .await
        .unwrap();
    let TerminalUpdate::Snapshot(source_initial) = read_terminal_update(&mut source.updates).await
    else {
        panic!("source terminal create must begin with a snapshot");
    };
    send_terminal_command(
        &source.commands,
        TerminalCommand::Input(b"printf 'SOURCE_CWD:%s\\n' \"$PWD\"\n".to_vec()),
    )
    .await;
    let source_needle = format!("SOURCE_CWD:{}", source_cwd.display());
    let _ = collect_terminal_frame_until(&mut source.updates, source_initial, &source_needle).await;

    let mut target_spec = terminal_spec(fallback_cwd.clone(), Some(source_id));
    target_spec.initial_size = wide;
    let mut target = client
        .hub
        .create_terminal(target_id, target_spec)
        .await
        .unwrap();
    let TerminalUpdate::Snapshot(target_initial) = read_terminal_update(&mut target.updates).await
    else {
        panic!("target terminal create must begin with a snapshot");
    };
    send_terminal_command(
        &target.commands,
        TerminalCommand::Input(b"printf 'TARGET_CWD:%s\\n' \"$PWD\"\n".to_vec()),
    )
    .await;
    let target_needle = format!("TARGET_CWD:{}", source_cwd.display());
    let _ = collect_terminal_frame_until(&mut target.updates, target_initial, &target_needle).await;

    send_terminal_command(&source.commands, TerminalCommand::Shutdown).await;
    send_terminal_command(&target.commands, TerminalCommand::Shutdown).await;
    std::fs::remove_dir_all(root).unwrap();
}

/// `new_agent` -> `UserMessage` -> the resulting events arrive over the
/// attachment's event channel in the same order the mock provider produced
/// them, forming a coherent transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_agent_then_user_message_streams_events_in_order() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "hello".to_string(),
        })
        .await
        .unwrap();

    let events = collect_events_until(&mut attachment.events, |event| {
        matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::Assistant && message.text == "Mock response: hello"
        )
    })
    .await;

    let user_message_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::MessageCommitted(message)
                    if message.role == MessageRole::User && message.text == "hello"
            )
        })
        .expect("the user message should have been committed");
    let assistant_reply_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::MessageCommitted(message)
                    if message.role == MessageRole::Assistant && message.text == "Mock response: hello"
            )
        })
        .expect("the assistant's reply should have been committed");
    assert!(
        assistant_reply_index > user_message_index,
        "the assistant's reply must land after the user's message, got: {events:?}"
    );
}

/// `list_agents` reflects a session created via `new_agent` on the same
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_agents_reflects_live_sessions_after_new_agent() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let _attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();

    assert_eq!(
        client.hub.list_agents().await.unwrap(),
        vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }]
    );
}

/// An auto-allow *host* tool (`workspace.snapshot`) executes sessiond-side
/// but can't answer itself -- it round-trips a host-tool request over the
/// connection-global channel (guardrail 4) and folds the client's response
/// into the same `ToolCallFinished` an ordinary auto tool would produce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_tool_executes_sessiond_side_via_host_tool_round_trip() {
    let sessiond = SessiondProcess::spawn();
    let mut client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please take a snapshot".to_string(),
        })
        .await
        .unwrap();

    let request = read_host_tool_request(&mut client).await;
    assert_eq!(request.tool_id, "workspace.snapshot");
    respond_host_tool(
        &client,
        HostToolResponse {
            request_id: request.request_id,
            output: serde_json::json!({ "tab_count": 1 }).into(),
        },
    )
    .await;

    let events = collect_events_until(
        &mut attachment.events,
        |event| matches!(event, Event::ToolCallFinished(result) if result.output["tab_count"] == 1),
    )
    .await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::ToolCallRequested(request) if request.tool_id == "workspace.snapshot"
        )),
        "expected the tool call to have been requested too, got: {events:?}"
    );
}

/// Approval round trip: an `ApprovalRequested` event flows out, an
/// `ApproveToolCall` command flows back in, and sessiond resolves it and
/// reports the result as an ordinary event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_round_trip_request_out_approve_in_result_event_out() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please run a tool".to_string(),
        })
        .await
        .unwrap();

    let events = collect_events_until(&mut attachment.events, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;
    let call_id = events
        .iter()
        .find_map(|event| match event {
            Event::ApprovalRequested(request) => Some(request.call_id.clone()),
            _ => None,
        })
        .expect("an approval request should have been observed");

    attachment
        .commands
        .send(AgentCommand::ApproveToolCall {
            call_id: call_id.clone(),
        })
        .await
        .unwrap();

    let events = collect_events_until(
        &mut attachment.events,
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted(id) if id == &call_id)),
        "approving should have started the tool call before finishing it, got: {events:?}"
    );
}

/// `bash` runs sessiond-side: approving a real `bash` tool call spawns an
/// actual subprocess in sessiond, and the eventual result arrives back over
/// the attachment's event channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_runs_sessiond_side_and_reports_its_result_over_the_wire() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please run bash".to_string(),
        })
        .await
        .unwrap();

    let events = collect_events_until(&mut attachment.events, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;
    let call_id = events
        .iter()
        .find_map(|event| match event {
            Event::ApprovalRequested(request) => Some(request.call_id.clone()),
            _ => None,
        })
        .expect("bash should request approval before running");

    attachment
        .commands
        .send(AgentCommand::ApproveToolCall {
            call_id: call_id.clone(),
        })
        .await
        .unwrap();

    let events = collect_events_until(
        &mut attachment.events,
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    )
    .await;
    let Some(Event::ToolCallFinished(result)) = events.iter().rev().find(
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    ) else {
        panic!("expected a ToolCallFinished event for {call_id:?}, got: {events:?}");
    };
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "sessiond-bash-ok\n");
}

/// Regression test for the 2026-07 repeated-approval OOM incident: 10
/// rapid duplicate `ApproveToolCall`s for the same still-running `bash`
/// call must start it exactly once -- both in observed events and the
/// persisted log -- because a session's commands are processed one at a
/// time on its own dedicated thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_rapid_approve_of_the_same_call_starts_bash_exactly_once() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please run bash".to_string(),
        })
        .await
        .unwrap();

    let events = collect_events_until(&mut attachment.events, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;
    let call_id = events
        .iter()
        .find_map(|event| match event {
            Event::ApprovalRequested(request) => Some(request.call_id.clone()),
            _ => None,
        })
        .expect("bash should request approval before running");

    for _ in 0..10 {
        attachment
            .commands
            .send(AgentCommand::ApproveToolCall {
                call_id: call_id.clone(),
            })
            .await
            .unwrap();
    }

    let events = collect_events_until(
        &mut attachment.events,
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    )
    .await;

    let started_count = events
        .iter()
        .filter(|event| matches!(event, Event::ToolCallStarted(id) if id == &call_id))
        .count();
    assert_eq!(
        started_count, 1,
        "10 rapid duplicate approvals must start the tool call exactly once, got: {events:?}"
    );
    let finished_count = events
        .iter()
        .filter(
            |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
        )
        .count();
    assert_eq!(
        finished_count, 1,
        "a duplicate approval must never produce a second result, got: {events:?}"
    );

    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read(&sessiond.event_log_path)
            .expect("the on-disk event log should parse cleanly");
        if candidate.records.iter().any(|record| {
            matches!(&record.event, Event::ToolCallFinished(result) if result.call_id == call_id)
        }) {
            report = Some(candidate);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let report = report.expect("the bash result should eventually be persisted");
    let logged_started_count = report
        .records
        .iter()
        .filter(|record| matches!(&record.event, Event::ToolCallStarted(id) if id == &call_id))
        .count();
    assert_eq!(
        logged_started_count, 1,
        "the persisted event log must contain exactly one ToolCallStarted for the call, got: {:?}",
        report.records
    );
}

/// The mock provider's `"streaming tool"` trigger emits ephemeral
/// tool-call-progress ticks before the real `ToolCallRequested` -- these
/// must still reach a connected client (now as
/// `AgentWireEvent::ToolCallProgress` on the attachment's event channel)
/// and must never appear in the durable on-disk event log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_tool_call_progress_reaches_the_client_but_never_the_event_log() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please use the streaming tool".to_string(),
        })
        .await
        .unwrap();

    let mut progress_ticks = Vec::new();
    let mut saw_tool_call_requested = false;
    for _ in 0..400 {
        let wire_event = tokio::time::timeout(Duration::from_secs(120), attachment.events.recv())
            .await
            .expect("timed out")
            .expect("channel error")
            .expect("the daemon should keep streaming events");
        match wire_event {
            AgentWireEvent::ToolCallProgress(progress) => progress_ticks.push(progress),
            AgentWireEvent::Event(Event::ToolCallRequested(request)) => {
                assert_eq!(request.tool_id, "mock.approval_required");
                saw_tool_call_requested = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        saw_tool_call_requested,
        "the real tool call request should follow the streamed preview"
    );
    assert!(
        progress_ticks.len() >= 3,
        "expected every mock streaming tick to reach the client as its own event, got: {progress_ticks:?}"
    );
    assert!(
        progress_ticks
            .windows(2)
            .all(|pair| pair[1].bytes >= pair[0].bytes),
        "byte counts should grow monotonically as the mock provider streams, got: {progress_ticks:?}"
    );

    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read(&sessiond.event_log_path)
            .expect("the on-disk event log should parse cleanly");
        let has_tool_call_requested = candidate.records.iter().any(|record| {
            matches!(
                &record.event,
                Event::ToolCallRequested(request) if request.tool_id == "mock.approval_required"
            )
        });
        if has_tool_call_requested {
            report = Some(candidate);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let report = report.expect("the real tool call request should eventually be persisted");
    assert_eq!(
        report.corrupt_line_count, 0,
        "every persisted line must still be a well-formed record, got: {report:?}"
    );

    let log_contents = std::fs::read_to_string(&sessiond.event_log_path)
        .expect("event log should exist and be readable");
    assert!(
        !log_contents
            .to_ascii_lowercase()
            .contains("tool_call_progress"),
        "the persisted event log must never contain the ephemeral tool-call-progress preview, got:\n{log_contents}"
    );
}

/// A corrupt line found during startup must be reported to a connecting
/// client once, on the `HubHello::skipped_lines` channel -- not just
/// printed to stderr -- so Horizon's status bar can surface it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_event_log_lines_are_reported_to_the_client_once_per_connection() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&event_log_path, "not valid json\n").expect("write corrupt fixture log");

    let sessiond = SessiondProcess::spawn_at(socket_path, event_log_path);
    let mut client = connect_hub(&sessiond.socket_path).await;

    let summary = tokio::time::timeout(Duration::from_secs(30), client.skipped_lines.recv())
        .await
        .expect("timed out waiting for the skipped-lines summary")
        .expect("skipped-lines channel error")
        .expect("the daemon should report its startup diagnostics on the skipped-lines channel");
    assert_eq!(summary, "skipped 1 corrupt line");
}

/// Step 4's headline scenario: `kill -9` a live daemon mid-session (a turn
/// genuinely still open in `WaitingForApproval`), respawn against the same
/// log, and confirm replay: transcript survives, the interrupted turn is
/// committed as cancelled, the session is immediately usable again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_sessiond_respawns_and_replays_transcript_with_open_turn_cancelled() {
    let sessiond = SessiondProcess::spawn();
    let socket_path = sessiond.socket_path.clone();
    let event_log_path = sessiond.event_log_path.clone();
    let client = connect_hub(&socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "please run a tool".to_string(),
        })
        .await
        .unwrap();

    collect_events_until(&mut attachment.events, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;
    wait_for_persisted_event(&event_log_path, session_id, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;

    drop(attachment);
    drop(client);
    sessiond.kill_and_wait();

    let respawned = SessiondProcess::spawn_at(socket_path, event_log_path);
    let client = connect_hub(&respawned.socket_path).await;

    assert_eq!(
        client.hub.list_agents().await.unwrap(),
        vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }],
        "the resumed session must be listed as live again"
    );

    let mut attachment = client.hub.attach_agent(session_id).await.unwrap();
    let replayed = collect_replayed_events(&mut attachment.events).await;

    assert!(
        replayed.iter().any(|event| matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::User && message.text == "please run a tool"
        )),
        "the pre-crash user message must survive replay, got: {replayed:?}"
    );
    assert!(
        replayed
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))),
        "the pre-crash approval request must survive replay, got: {replayed:?}"
    );
    assert!(
        replayed
            .iter()
            .any(|event| matches!(event, Event::TurnEnded(TurnEndReason::Cancelled))),
        "the interrupted turn must be committed as cancelled on resume, got: {replayed:?}"
    );
    let frame = agent_frame_from_events(&replayed);
    assert!(
        !frame.is_turn_in_flight(),
        "replay must leave the session ready for a new turn, got frame: {frame:?}"
    );
    assert!(
        frame.pending_approval_call_id().is_none(),
        "the cancelled approval must not still read as pending, got frame: {frame:?}"
    );

    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "hello again".to_string(),
        })
        .await
        .unwrap();
    let events = collect_events_until(&mut attachment.events, |event| {
        matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::Assistant && message.text == "Mock response: hello again"
        )
    })
    .await;
    assert!(events.iter().any(|event| matches!(
        event,
        Event::MessageCommitted(message)
            if message.role == MessageRole::User && message.text == "hello again"
    )));
}

/// A crash-and-respawn must restore a session's role, not just its
/// provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_restores_the_sessions_role_after_a_crash_and_respawn() {
    let sessiond = SessiondProcess::spawn();
    let socket_path = sessiond.socket_path.clone();
    let event_log_path = sessiond.event_log_path.clone();
    let client = connect_hub(&socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client
        .hub
        .new_agent(session_new_with_role(
            session_id,
            RoleId("config".to_string()),
        ))
        .await
        .unwrap();
    // Drain the startup burst until its init message reaches the wire...
    collect_events_until(&mut attachment.events, |event| {
        matches!(event, Event::MessageCommitted(_))
    })
    .await;
    // ...and disk, before the hard kill.
    wait_for_persisted_event(&event_log_path, session_id, |event| {
        matches!(event, Event::MessageCommitted(_))
    })
    .await;

    drop(attachment);
    drop(client);
    sessiond.kill_and_wait();

    let respawned = SessiondProcess::spawn_at(socket_path, event_log_path);
    let client = connect_hub(&respawned.socket_path).await;
    assert_eq!(
        client.hub.list_agents().await.unwrap(),
        vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
            role_id: Some(RoleId("config".to_string())),
            parent_session_id: None,
            workspace_root: None,
        }],
        "resume must restore the session's role, not just its provider"
    );
}

/// `attach_agent` bootstrap (no crash): a client that disconnects and
/// reconnects to the same running daemon must see the session's frame come
/// back identical to the one it had live.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_agent_after_reconnect_rebuilds_an_equivalent_frame() {
    let sessiond = SessiondProcess::spawn();
    let client = connect_hub(&sessiond.socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "hello".to_string(),
        })
        .await
        .unwrap();

    let mut seen_reply = false;
    let live_events = collect_events_until(&mut attachment.events, |event| {
        if matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::Assistant && message.text == "Mock response: hello"
        ) {
            seen_reply = true;
        }
        seen_reply && matches!(event, Event::StateChanged(SessionState::WaitingForUser))
    })
    .await;
    let live_frame = agent_frame_from_events(&live_events);

    // Disconnect without draining -- the session keeps running.
    drop(attachment);
    drop(client);

    let client = connect_hub(&sessiond.socket_path).await;
    let mut attachment = client.hub.attach_agent(session_id).await.unwrap();
    let replayed_events = collect_replayed_events(&mut attachment.events).await;
    let replayed_frame = agent_frame_from_events(&replayed_events);

    assert_eq!(
        replayed_frame, live_frame,
        "attach_agent's replay must fold to the exact same frame the live connection saw"
    );
}

/// The server-side substance of `Reload Session Runtime`: drain a live
/// session gracefully (not a crash), respawn against the same paths, and
/// confirm the session survives with its transcript intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drained_sessiond_respawns_and_preserves_a_completed_session() {
    let mut sessiond = SessiondProcess::spawn();
    let socket_path = sessiond.socket_path.clone();
    let event_log_path = sessiond.event_log_path.clone();
    let client = connect_hub(&socket_path).await;

    let session_id = SessionId::new();
    let mut attachment = client.hub.new_agent(session_new(session_id)).await.unwrap();
    attachment
        .commands
        .send(AgentCommand::UserMessage {
            text: "hello".to_string(),
        })
        .await
        .unwrap();
    collect_events_until(&mut attachment.events, |event| {
        matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::Assistant && message.text == "Mock response: hello"
        )
    })
    .await;

    client.drain().await;
    drop(attachment);
    drop(client);
    let status = wait_for_exit(&mut sessiond.child).await;
    assert!(status.success(), "drain should exit 0, got {status:?}");

    let respawned = SessiondProcess::spawn_at(socket_path, event_log_path);
    let client = connect_hub(&respawned.socket_path).await;
    assert_eq!(
        client.hub.list_agents().await.unwrap(),
        vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }],
        "a gracefully drained session must resume too, not just a crashed one"
    );

    let mut attachment = client.hub.attach_agent(session_id).await.unwrap();
    let replayed = collect_replayed_events(&mut attachment.events).await;
    assert!(
        replayed.iter().any(|event| matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::User && message.text == "hello"
        )),
        "the pre-drain transcript must survive, got: {replayed:?}"
    );
    assert!(
        !replayed
            .iter()
            .any(|event| matches!(event, Event::TurnEnded(TurnEndReason::Cancelled))),
        "a turn that had already completed cleanly before the drain must not be \
         re-marked as cancelled on resume, got: {replayed:?}"
    );
}

/// Fix 2: a session whose log already ends in a terminal state must not be
/// resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_skips_sessions_whose_log_already_ended_in_a_terminal_state() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let terminated_session = SessionId::new();
    let exited_session = SessionId::new();
    let live_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![
            (
                terminated_session,
                vec![
                    Event::StateChanged(SessionState::Created),
                    Event::StateChanged(SessionState::WaitingForUser),
                    Event::StateChanged(SessionState::Terminated),
                ],
            ),
            (
                exited_session,
                vec![
                    Event::StateChanged(SessionState::Created),
                    Event::StateChanged(SessionState::WaitingForUser),
                    Event::StateChanged(SessionState::Terminated),
                    Event::Exited(Exit {
                        reason: "shutdown".to_string(),
                    }),
                ],
            ),
            (
                live_session,
                vec![
                    Event::StateChanged(SessionState::Created),
                    Event::StateChanged(SessionState::WaitingForUser),
                ],
            ),
        ],
    );

    let sessiond = SessiondProcess::spawn_at(socket_path, event_log_path);
    let client = connect_hub(&sessiond.socket_path).await;
    assert_eq!(
        client.hub.list_agents().await.unwrap(),
        vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }],
        "only the live session should have been resumed"
    );
}

/// Fix 1: `hello` must answer well before a slow resume finishes, and
/// `list_agents` must wait for it -- proven with the resume-delay hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_answers_immediately_while_list_agents_waits_for_a_slow_resume() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let live_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            live_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    const RESUME_DELAY_MS: u64 = 2000;
    let sessiond = SessiondProcess::spawn_at_with_resume_delay(
        socket_path,
        event_log_path,
        Some(RESUME_DELAY_MS),
    );

    let hello_started = Instant::now();
    let client = connect_hub(&sessiond.socket_path).await;
    let hello_elapsed = hello_started.elapsed();
    assert!(
        hello_elapsed < Duration::from_millis(RESUME_DELAY_MS / 2),
        "hello should answer well before the artificial resume delay elapses, took {hello_elapsed:?}"
    );

    let list_started = Instant::now();
    let agents = client.hub.list_agents().await.unwrap();
    let list_elapsed = list_started.elapsed();
    assert!(
        list_elapsed >= Duration::from_millis(RESUME_DELAY_MS) - Duration::from_millis(300),
        "list_agents should have waited for the (artificially slow) resume to finish, took {list_elapsed:?}"
    );
    assert_eq!(
        agents,
        vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }]
    );
}

/// Fix 1's other half: a second daemon against a live socket must bail
/// before reading its own log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_sessiond_against_a_live_socket_exits_before_reading_its_own_log() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let live_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            live_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    let first = SessiondProcess::spawn_at(socket_path.clone(), event_log_path.clone());
    // Wait for the first instance to be up and resumed (list_agents' own
    // readiness gate) before racing a second one against it.
    let client = connect_hub(&first.socket_path).await;
    let _ = client.hub.list_agents().await.unwrap();
    drop(client);

    let missing_config_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-no-such-config-{}-{}.toml",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-state-{}-{}.duckdb",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut second_command = Command::new(resolve_sessiond_binary());
    second_command
        .arg("--socket")
        .arg(&socket_path)
        .env("HORIZON_CONFIG", &missing_config_path)
        .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
        .env("HORIZON_AGENT_STATE_DB", &state_db_path)
        .env_remove(TEST_RESUME_DELAY_MS_VAR)
        .stderr(Stdio::piped());
    let mut second = spawn_sessiond(&mut second_command);

    let status = wait_for_exit(&mut second).await;
    assert!(
        !status.success(),
        "a second instance against a live socket must exit non-zero, got {status:?}"
    );

    let mut stderr = String::new();
    second
        .stderr
        .take()
        .expect("stderr should have been piped")
        .read_to_string(&mut stderr)
        .expect("read second instance's stderr");
    assert!(
        stderr.contains("already accepting connections"),
        "expected the live-socket bail message, stderr was: {stderr}"
    );
    assert!(
        !stderr.contains("resumed session"),
        "the second instance must bail before reading/resuming its own log, stderr was: {stderr}"
    );

    drop(first);
}

/// Task 1: `hello`/`list_agents` must both answer promptly even while an
/// (artificially slowed) DuckDB rebuild is still running in the background.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duckdb_rebuild_delay_does_not_block_hello_or_list_agents() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let live_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            live_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    const REBUILD_DELAY_MS: u64 = 2000;
    let sessiond = SessiondProcess::spawn_at_with_duckdb_rebuild_delay(
        socket_path,
        event_log_path,
        REBUILD_DELAY_MS,
    );

    let hello_started = Instant::now();
    let client = connect_hub(&sessiond.socket_path).await;
    let hello_elapsed = hello_started.elapsed();
    assert!(
        hello_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "hello should answer well before the artificial duckdb rebuild delay elapses, took {hello_elapsed:?}"
    );

    let list_started = Instant::now();
    let agents = client.hub.list_agents().await.unwrap();
    let list_elapsed = list_started.elapsed();
    assert!(
        list_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "list_agents must not wait on the (slow) duckdb rebuild, took {list_elapsed:?}"
    );
    assert_eq!(
        agents,
        vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }]
    );
}

/// Task 2's skip path: a second spawn against an *unchanged* event log must
/// skip the DuckDB rebuild entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_log_skips_duckdb_rebuild_on_respawn() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-state-{}-{}.duckdb",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let session_id = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            session_id,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
                Event::StateChanged(SessionState::Terminated),
            ],
        )],
    );

    let first = SessiondProcess::spawn_at_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    drop(connect_hub(&first.socket_path).await);
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

    let second =
        SessiondProcess::spawn_at_with_duckdb_options(socket_path, event_log_path, state_db_path);
    drop(connect_hub(&second.socket_path).await);
    second
        .wait_for_stderr_line("DuckDB projection already current, skipping rebuild")
        .await;
}

/// Task 2's other half: a log that grew since the projection was last built
/// must still be reconciled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_log_triggers_duckdb_rebuild_on_respawn() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-sessiond-e2e-state-{}-{}.duckdb",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let first_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            first_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    let first = SessiondProcess::spawn_at_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    drop(connect_hub(&first.socket_path).await);
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

    let second_session = SessionId::new();
    write_session_fixture(
        &event_log_path,
        vec![(
            second_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    let second =
        SessiondProcess::spawn_at_with_duckdb_options(socket_path, event_log_path, state_db_path);
    drop(connect_hub(&second.socket_path).await);
    let catch_up_line = second
        .wait_for_stderr_line("DuckDB projection caught up incrementally (")
        .await;
    assert!(
        !catch_up_line.contains("already current"),
        "a stale (grown) log must trigger real reconciliation work, not the skip path: {catch_up_line}"
    );
}
