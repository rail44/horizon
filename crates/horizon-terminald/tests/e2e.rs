//! End-to-end tests against the real `horizon-terminald` binary, over the
//! actual `TerminalHub` rtc trait on an actual unix socket: `hello` range
//! negotiation, `create_terminal`/`attach_terminal` returning
//! channel-bearing attachments, real PTYs backed by a real `/bin/sh`, and
//! `drain`.
//!
//! Moved here (with the daemon) from `horizon-agentd`'s e2e suite by the
//! v17 terminald split (`docs/terminald-split-design.md`), plus the split's
//! **acceptance test**:
//! [`an_agentd_drain_and_respawn_leaves_a_live_terminald_session_attachable`]
//! spawns *both* daemons and proves that the whole `Reload Agent Runtime`
//! sequence — graceful drain, process exit, respawn — leaves a terminald
//! session attachable with its retained frame and a live shell. That is the
//! property the split exists for, and the one thing no unit test can show.
//!
//! The tests use a multi-thread runtime because the remoc chmux mux task
//! must be polled concurrently with the test's own awaits (adoption
//! condition 3) while some helpers block briefly (process spawn/kill).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

use horizon_agent::wire::{agent_client_hello, SessionHub as _, SessionHubClient};
use horizon_terminal_core::wire::{
    terminal_client_hello, TerminalHub as _, TerminalHubClient,
    MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION, TERMINAL_PROTOCOL_VERSION,
};
use horizon_terminal_core::{
    TerminalColorScheme, TerminalCommand, TerminalFrame, TerminalSize, TerminalSpawnSpec,
    TerminalSummary, TerminalUpdate,
};
use horizon_wire::{
    CappedReceiver, CappedWatchReceiver, ClientHello, HubError, VersionRange, WireCodec,
    FRAME_MAX_ITEM_BYTES, TERMINAL_EVENT_MAX_ITEM_BYTES,
};
use remoc::rch;
use tokio::net::UnixStream;

/// See `horizon-agentd`'s e2e suite for the full write-up of why the
/// *runtime* env var is preferred over the same-named compile-time `env!()`
/// bake (`docs/tasks/backlog.md` #40): under this repo's shared build-dir a
/// cached test binary's baked path can point into a deleted worktree, while
/// cargo/nextest inject the runtime value fresh for every invocation.
const CARGO_BIN_EXE_VAR: &str = "CARGO_BIN_EXE_horizon-terminald";

/// How long to wait once before re-probing/re-spawning after finding a
/// daemon binary transiently missing — cargo's own non-atomic artifact
/// uplift window (`docs/tasks/backlog.md` #36).
const TRANSIENT_LINK_RETRY_DELAY: Duration = Duration::from_millis(200);

const TERMINAL_UPDATE_TIMEOUT: Duration = Duration::from_secs(120);

fn resolve_terminald_binary() -> PathBuf {
    if let Ok(runtime_var) = std::env::var(CARGO_BIN_EXE_VAR) {
        let path = PathBuf::from(runtime_var);
        if path.is_file() {
            return path;
        }
    }
    let baked_in = PathBuf::from(env!("CARGO_BIN_EXE_horizon-terminald"));
    if baked_in.is_file() {
        return baked_in;
    }
    panic!(
        "could not locate the horizon-terminald binary to spawn for this e2e test -- probed \
         runtime env var {CARGO_BIN_EXE_VAR} = {:?} and compile-time bake = {} (exists = {}) \
         -- see docs/tasks/backlog.md #40",
        std::env::var(CARGO_BIN_EXE_VAR),
        baked_in.display(),
        baked_in.is_file(),
    );
}

/// `horizon-agentd` for the cross-daemon acceptance test, resolved as a
/// *sibling* of the terminald binary: `CARGO_BIN_EXE_<name>` is only
/// injected for binaries of the package a test belongs to, and every
/// workspace binary is uplifted into the same target directory, so the
/// sibling lookup is the one honest way to reach it from here (the same rule
/// `horizon_agent::client::resolve_daemon_binary` uses in production).
fn resolve_agentd_binary() -> PathBuf {
    let terminald = resolve_terminald_binary();
    let candidate = terminald
        .parent()
        .expect("the terminald binary must live in a directory")
        .join("horizon-agentd");
    assert!(
        candidate.is_file(),
        "expected horizon-agentd next to horizon-terminald at {} -- run \
         `cargo build --workspace` (the quality gate does)",
        candidate.display()
    );
    candidate
}

fn spawn_daemon(command: &mut Command) -> Child {
    match command.spawn() {
        Ok(child) => child,
        Err(first_error) if first_error.kind() == ErrorKind::NotFound => {
            thread::sleep(TRANSIENT_LINK_RETRY_DELAY);
            command.spawn().unwrap_or_else(|retry_error| {
                let program = command.get_program().to_owned();
                panic!(
                    "failed to spawn {} even after a retry for a transient link window: \
                     first error = {first_error}, retry error = {retry_error} -- see \
                     docs/tasks/backlog.md #36",
                    program.to_string_lossy(),
                )
            })
        }
        Err(error) => panic!("failed to spawn a daemon: {error}"),
    }
}

/// A short, SUN_LEN-safe socket path in the system temp dir.
fn scratch_socket(tag: &str) -> PathBuf {
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    std::env::temp_dir().join(format!("hzn-{tag}-{short_id}.sock"))
}

/// Owns a spawned `horizon-terminald` and its socket path; kills the child
/// and removes the socket file on drop so a failing assertion doesn't leak
/// either (nor the PTYs the daemon owns) across test runs.
struct TerminaldProcess {
    child: Child,
    socket_path: PathBuf,
}

impl TerminaldProcess {
    fn spawn() -> Self {
        let socket_path = scratch_socket("td-e2e");
        let mut command = Command::new(resolve_terminald_binary());
        command.arg("--socket").arg(&socket_path);
        Self {
            child: spawn_daemon(&mut command),
            socket_path,
        }
    }
}

impl Drop for TerminaldProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// The acceptance test's `horizon-agentd`, spawned hermetically: its own
/// throwaway event log and DuckDB projection plus a nonexistent config
/// file, so it never reads or rebuilds a real developer's agent state.
struct AgentdProcess {
    child: Child,
    socket_path: PathBuf,
    event_log_path: PathBuf,
    state_db_path: PathBuf,
}

impl AgentdProcess {
    fn spawn() -> Self {
        let socket_path = scratch_socket("sd-for-td-e2e");
        let unique = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let event_log_path =
            std::env::temp_dir().join(format!("horizon-terminald-e2e-events-{unique}.jsonl"));
        let state_db_path =
            std::env::temp_dir().join(format!("horizon-terminald-e2e-state-{unique}.duckdb"));
        Self::spawn_at(socket_path, event_log_path, state_db_path)
    }

    fn spawn_at(socket_path: PathBuf, event_log_path: PathBuf, state_db_path: PathBuf) -> Self {
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-terminald-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(resolve_agentd_binary());
        command
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        Self {
            child: spawn_daemon(&mut command),
            socket_path,
            event_log_path,
            state_db_path,
        }
    }

    /// Respawns a fresh process at the same paths — what `Reload Session
    /// Runtime` does after the drain completes.
    fn respawn_at_same_paths(self) -> Self {
        let socket_path = self.socket_path.clone();
        let event_log_path = self.event_log_path.clone();
        let state_db_path = self.state_db_path.clone();
        // Skip `Drop` (which would remove the paths): the point is to bring a
        // new process up on exactly the same socket and log.
        std::mem::forget(self);
        Self::spawn_at(socket_path, event_log_path, state_db_path)
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

async fn connect_with_retry(path: &Path) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no daemon ever accepted a connection on {}", path.display());
}

async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    for _ in 0..200 {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the daemon did not exit in time");
}

// --- the terminal hub test harness -----------------------------------------

/// A connected `TerminalHub` client over the real socket. Owns the chmux mux
/// task (aborted on drop, which closes the socket so the daemon's
/// one-at-a-time accept loop can serve the next connection).
struct HubTestClient {
    hub: TerminalHubClient<WireCodec>,
    negotiated: u32,
    binary_id: String,
    conn_task: tokio::task::JoinHandle<()>,
}

impl Drop for HubTestClient {
    fn drop(&mut self) {
        self.conn_task.abort();
    }
}

/// Connects and hands the *unnegotiated* pieces back, so a caller can
/// either negotiate (see [`connect_hub`]) or deliberately fail negotiation
/// and keep using the same connection for the version-stable `drain` — the
/// recovery path `Reload Terminal Runtime` depends on.
async fn connect_raw(
    stream: UnixStream,
) -> (TerminalHubClient<WireCodec>, tokio::task::JoinHandle<()>) {
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), TerminalHubClient<WireCodec>, WireCodec>(
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
        .expect("the daemon should hand over its hub client");
    (hub, conn_task)
}

async fn connect_hub(socket_path: &Path) -> HubTestClient {
    let stream = connect_with_retry(socket_path).await;
    let (hub, conn_task) = connect_raw(stream).await;
    let hello = hub
        .hello(terminal_client_hello("terminald-e2e"))
        .await
        .expect("hello should succeed at a matching version range");
    HubTestClient {
        hub,
        negotiated: hello.negotiated,
        binary_id: hello.binary_id,
        conn_task,
    }
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

/// The attachment's current frame — the watch's seed on attach (the
/// daemon-retained latest frame), or the newest published frame.
fn current_frame(
    frames: &CappedWatchReceiver<TerminalFrame, FRAME_MAX_ITEM_BYTES>,
) -> TerminalFrame {
    frames.borrow().expect("frame watch error").clone()
}

/// Waits for the attachment to report that its session has ended after a
/// `Shutdown`, skipping whatever non-exit events (title, bell, a final
/// error) arrive first.
///
/// Two outcomes count as "ended", because the daemon produces them from the
/// same critical section and production treats them identically
/// (`run_terminal_attachment`'s `Exited` and `Ok(None)` arms both retire the
/// pane): an explicit `TerminalUpdate::Exited`, or the events channel
/// closing — `forward_updates` removes the subscriber and sends the exit
/// under one lock, so a client can legitimately observe the close instead of
/// the message. A timeout panics with everything that *was* seen, so a
/// future flake says what the daemon did rather than only `Elapsed`.
async fn wait_for_session_end(
    events: &mut CappedReceiver<TerminalUpdate, TERMINAL_EVENT_MAX_ITEM_BYTES>,
) {
    let deadline = tokio::time::Instant::now() + TERMINAL_UPDATE_TIMEOUT;
    let mut seen = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(Some(TerminalUpdate::Exited))) => return,
            Ok(Ok(Some(other))) => seen.push(format!("{other:?}")),
            // The attachment is gone -- the session ended with it.
            Ok(Ok(None)) => return,
            Ok(Err(error)) if error.is_final() => return,
            Ok(Err(error)) => seen.push(format!("skipped an undecodable item: {error}")),
            Err(_elapsed) => panic!(
                "the session never reported an exit within {TERMINAL_UPDATE_TIMEOUT:?} after \
                 Shutdown; events seen meanwhile: {seen:?}"
            ),
        }
    }
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
        control_socket: "/tmp/horizon-terminald-e2e-control.sock".into(),
        fallback_cwd,
        spawn_source_session_id,
        initial_size: TerminalSize::new(80, 24),
    }
}

/// Folds the attachment's frame watch toward a frame whose text contains
/// `needle`, returning it. Checks the current value first (the seed on
/// attach), then awaits changes; the watch's latest-value semantics mean a
/// slow reader skips intermediate frames and still converges on the needle.
async fn collect_terminal_frame_until(
    frames: &mut CappedWatchReceiver<TerminalFrame, FRAME_MAX_ITEM_BYTES>,
    needle: &str,
) -> TerminalFrame {
    for _ in 0..1000 {
        {
            let frame = frames
                .borrow_and_update()
                .expect("frame watch error")
                .clone();
            if frame.text().contains(needle) {
                return frame;
            }
        }
        tokio::time::timeout(TERMINAL_UPDATE_TIMEOUT, frames.changed())
            .await
            .expect("timed out waiting for a terminal frame")
            .expect("the frame watch closed before the needle arrived");
    }
    panic!(
        "gave up waiting for {needle:?}; last frame: {:?}",
        current_frame(frames).text()
    );
}

// --- tests -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_negotiates_reports_the_binary_id_and_drains_over_the_real_socket() {
    let mut terminald = TerminaldProcess::spawn();
    let client = connect_hub(&terminald.socket_path).await;

    assert_eq!(client.negotiated, TERMINAL_PROTOCOL_VERSION);
    assert!(
        client.binary_id.starts_with("horizon-terminald/"),
        "the daemon must identify itself for the skew message (decision 6), got {:?}",
        client.binary_id
    );
    assert_eq!(client.hub.list_terminals().await.unwrap(), Vec::new());

    client.drain().await;
    let status = wait_for_exit(&mut terminald.child).await;
    assert_eq!(
        status.code(),
        Some(0),
        "horizon-terminald should exit 0 after a drain, got {status:?}"
    );
}

/// `hello` and `drain` are the version-stable surface: a client whose range
/// does not overlap the daemon's is rejected, but can still drain it -- the
/// path `Reload Terminal Runtime`'s automatic recovery depends on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_version_range_is_rejected_but_drain_still_works() {
    let mut terminald = TerminaldProcess::spawn();
    let stream = connect_with_retry(&terminald.socket_path).await;
    let (hub, conn_task) = connect_raw(stream).await;

    let future = VersionRange {
        min_supported: TERMINAL_PROTOCOL_VERSION + 100,
        current: TERMINAL_PROTOCOL_VERSION + 100,
    };
    let result = hub
        .hello(ClientHello {
            supported: future,
            binary_id: "future-horizon".to_string(),
        })
        .await;
    match result {
        Err(HubError::IncompatibleVersion { client, daemon }) => {
            assert_eq!(client, future);
            assert_eq!(daemon.current, TERMINAL_PROTOCOL_VERSION);
            assert_eq!(
                daemon.min_supported,
                MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION
            );
        }
        Err(other) => panic!("expected a version rejection, got {other:?}"),
        Ok(_) => panic!("a disjoint version range must be rejected"),
    }

    // The version-stable `drain` still works on the same connection -- the
    // rejected client's one legitimate move, and what the client runtime's
    // stale-terminald recovery relies on.
    let _ = tokio::time::timeout(Duration::from_secs(5), hub.drain()).await;
    let status = wait_for_exit(&mut terminald.child).await;
    assert!(
        status.success(),
        "horizon-terminald should exit 0 after a post-rejection drain, got {status:?}"
    );
    conn_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_create_frame_reconnect_attach_and_shutdown_over_the_real_socket() {
    let terminald = TerminaldProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let client = connect_hub(&terminald.socket_path).await;

    let mut attachment = client
        .hub
        .create_terminal(session_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .expect("create should succeed");

    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'HORIZON_DIFF_MARKER\\n'\n".to_vec()),
    )
    .await;
    // Full frames stream on the watch and converge on the marker.
    let frame = collect_terminal_frame_until(&mut attachment.frames, "HORIZON_DIFF_MARKER").await;
    assert!(frame.text().contains("HORIZON_DIFF_MARKER"));

    // Disconnect this client entirely; the terminal session keeps running
    // (process-scoped), so a fresh connection can reattach.
    drop(attachment);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
    let attachment = client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("attach on a fresh connection should succeed");
    // The reattach reseeds the full retained latest frame structurally: the
    // watch's current value already carries the marker, with no snapshot
    // request or baseline dance (§5 Option A).
    let attached = current_frame(&attachment.frames);
    assert!(
        attached.text().contains("HORIZON_DIFF_MARKER"),
        "attach must reseed the retained latest frame, got: {:?}",
        attached.text()
    );

    let mut attachment = attachment;
    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'HORIZON_REATTACH_MARKER\\n'\n".to_vec()),
    )
    .await;
    let reattached =
        collect_terminal_frame_until(&mut attachment.frames, "HORIZON_REATTACH_MARKER").await;
    assert!(reattached.text().contains("HORIZON_REATTACH_MARKER"));

    send_terminal_command(&attachment.commands, TerminalCommand::Shutdown).await;
    wait_for_session_end(&mut attachment.events).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_list_is_sorted_and_a_missing_attach_is_explicit() {
    let terminald = TerminaldProcess::spawn();
    let high_id = uuid::Uuid::from_u128(2);
    let low_id = uuid::Uuid::from_u128(1);

    let client = connect_hub(&terminald.socket_path).await;
    assert_eq!(client.hub.list_terminals().await.unwrap(), Vec::new());

    // Create two terminals across two connections; both survive the
    // disconnect (process-scoped sessions).
    let high = client
        .hub
        .create_terminal(high_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    // The attachment's frame watch carries a seed frame immediately.
    let _ = current_frame(&high.frames);
    drop(high);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
    let low = client
        .hub
        .create_terminal(low_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    let _ = current_frame(&low.frames);
    drop(low);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
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
    let terminald = TerminaldProcess::spawn();
    let root = std::env::temp_dir().join(format!("hzn-cwd-e2e-{}", uuid::Uuid::new_v4()));
    let source_cwd = root.join("source");
    let fallback_cwd = root.join("fallback");
    std::fs::create_dir_all(&source_cwd).unwrap();
    std::fs::create_dir_all(&fallback_cwd).unwrap();
    let source_cwd = source_cwd.canonicalize().unwrap();
    let wide = TerminalSize::new(200, 24);

    let source_id = uuid::Uuid::new_v4();
    let target_id = uuid::Uuid::new_v4();
    let client = connect_hub(&terminald.socket_path).await;

    let mut source_spec = terminal_spec(source_cwd.clone(), None);
    source_spec.initial_size = wide;
    let mut source = client
        .hub
        .create_terminal(source_id, source_spec)
        .await
        .unwrap();
    send_terminal_command(
        &source.commands,
        TerminalCommand::Input(b"printf 'SOURCE_CWD:%s\\n' \"$PWD\"\n".to_vec()),
    )
    .await;
    let source_needle = format!("SOURCE_CWD:{}", source_cwd.display());
    let _ = collect_terminal_frame_until(&mut source.frames, &source_needle).await;

    let mut target_spec = terminal_spec(fallback_cwd.clone(), Some(source_id));
    target_spec.initial_size = wide;
    let mut target = client
        .hub
        .create_terminal(target_id, target_spec)
        .await
        .unwrap();
    send_terminal_command(
        &target.commands,
        TerminalCommand::Input(b"printf 'TARGET_CWD:%s\\n' \"$PWD\"\n".to_vec()),
    )
    .await;
    let target_needle = format!("TARGET_CWD:{}", source_cwd.display());
    let _ = collect_terminal_frame_until(&mut target.frames, &target_needle).await;

    send_terminal_command(&source.commands, TerminalCommand::Shutdown).await;
    send_terminal_command(&target.commands, TerminalCommand::Shutdown).await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Drains a `horizon-agentd` over its own hub — exactly what
/// `Reload Agent Runtime` sends (`AgentdHandle::begin_reload` →
/// `SessionHub::drain`). The reply never travels (the daemon exits inside
/// the call), so the transport error is expected; the caller confirms the
/// exit with [`wait_for_exit`].
async fn drain_agentd(socket_path: &Path) {
    let stream = connect_with_retry(socket_path).await;
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), SessionHubClient<WireCodec>, WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        )
        .await
        .expect("remoc connect to agentd");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let hub = base_rx
        .recv()
        .await
        .expect("base channel recv")
        .expect("agentd should hand over its hub client");
    hub.hello(agent_client_hello("terminald-e2e"))
        .await
        .expect("hello should succeed at a matching version range");
    let _ = tokio::time::timeout(Duration::from_secs(5), hub.drain()).await;
    conn_task.abort();
}

/// **The terminald split's acceptance property**
/// (`docs/terminald-split-design.md` decisions 1-2): the exact sequence
/// `Reload Agent Runtime` performs against the *agent* daemon — graceful
/// rtc `drain`, wait for the process to exit, spawn a fresh one on the same
/// socket — leaves a `horizon-terminald` session fully usable: still listed,
/// still attachable, its retained frame intact, and its shell still alive
/// (proven by making the same PTY echo a *new* marker afterwards).
///
/// Before the split this was impossible by construction: one process owned
/// both, and its drain killed every PTY (`SessionHub::drain` called
/// `TerminalHost::shutdown_all`). The test is deliberately end-to-end over
/// two real daemons on two real sockets, because that separation is the
/// whole deliverable — the client-side half is pinned separately in
/// `src/agentd/tests.rs`
/// (`draining_the_agent_runtime_leaves_the_terminal_runtime_untouched`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agentd_drain_and_respawn_leaves_a_live_terminald_session_attachable() {
    let terminald = TerminaldProcess::spawn();
    let mut agentd = AgentdProcess::spawn();

    // A live terminal session with recognizable output.
    let session_id = uuid::Uuid::new_v4();
    let terminal_client = connect_hub(&terminald.socket_path).await;
    let mut attachment = terminal_client
        .hub
        .create_terminal(session_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .expect("create should succeed");
    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'BEFORE_AGENT_RELOAD\\n'\n".to_vec()),
    )
    .await;
    let _ = collect_terminal_frame_until(&mut attachment.frames, "BEFORE_AGENT_RELOAD").await;

    // `Reload Agent Runtime`, faithfully: drain the agent daemon over its
    // own hub, confirm the process is gone, then bring a fresh one up on the
    // same socket and event log.
    drain_agentd(&agentd.socket_path).await;
    let status = wait_for_exit(&mut agentd.child).await;
    assert_eq!(
        status.code(),
        Some(0),
        "horizon-agentd should exit 0 after a drain, got {status:?}"
    );
    let agentd = agentd.respawn_at_same_paths();
    // The replacement really is up (its bind-first startup makes this
    // immediate), so the reload sequence is complete, not merely started.
    drop(connect_with_retry(&agentd.socket_path).await);

    // The terminal daemon never noticed. Its session is still listed...
    assert_eq!(
        terminal_client.hub.list_terminals().await.unwrap(),
        vec![TerminalSummary { session_id }],
        "the agent daemon's restart must not remove a terminal session"
    );
    // ...the existing attachment still carries the pre-reload frame...
    assert!(
        current_frame(&attachment.frames)
            .text()
            .contains("BEFORE_AGENT_RELOAD"),
        "the retained frame must survive the agent daemon's restart"
    );
    // ...a *fresh* attachment reseeds it (the UI-restart path)...
    let reattached = terminal_client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("the session must still be attachable after the agent reload");
    assert!(
        current_frame(&reattached.frames)
            .text()
            .contains("BEFORE_AGENT_RELOAD"),
        "a re-attach after the agent reload must reseed the retained frame"
    );
    // ...and the shell itself is still alive, which is the part that
    // actually mattered to the operator: it answers new input.
    let mut reattached = reattached;
    send_terminal_command(
        &reattached.commands,
        TerminalCommand::Input(b"printf 'AFTER_AGENT_RELOAD\\n'\n".to_vec()),
    )
    .await;
    let after = collect_terminal_frame_until(&mut reattached.frames, "AFTER_AGENT_RELOAD").await;
    assert!(
        after.text().contains("AFTER_AGENT_RELOAD"),
        "the PTY's shell must still be running after the agent daemon restart"
    );

    send_terminal_command(&reattached.commands, TerminalCommand::Shutdown).await;
}
