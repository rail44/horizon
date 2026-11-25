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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use horizon_daemon_testkit::{
    AgentdPaths, AgentdProcess, AgentdSpawn, DaemonProcess, connect_hub_client, connect_with_retry,
    drain_with_timeout, resolve_daemon_binary, scratch_socket, sibling_daemon_binary,
    wait_for_exit,
};
use horizon_terminal_core::wire::TerminalAttachment;
use horizon_terminal_core::wire::{
    MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION, TERMINAL_PROTOCOL_VERSION, TerminalHub as _,
    TerminalHubClient, terminal_client_hello,
};
use horizon_terminal_core::{
    TerminalColorScheme, TerminalCommand, TerminalCore, TerminalSize, TerminalSpawnSpec,
    TerminalSummary, TerminalUpdate,
};
use horizon_wire::{
    CappedReceiver, ClientHello, HubError, TERMINAL_EVENT_MAX_ITEM_BYTES, VersionRange, WireCodec,
};
use remoc::rch;
use tokio::net::UnixStream;

const TERMINAL_UPDATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolves the `horizon-terminald` binary to spawn. Only the `env!()` bake
/// has to be produced here (that macro expands only inside the package that
/// owns the `[[bin]]` target); the resolution rule and its rationale live in
/// `horizon_daemon_testkit::binary`.
fn resolve_terminald_binary() -> PathBuf {
    resolve_daemon_binary("horizon-terminald", env!("CARGO_BIN_EXE_horizon-terminald"))
}

/// Spawns `horizon-terminald` on a fresh throwaway socket. The daemon owns
/// no persistence, so the socket is the only scratch state to clean up --
/// the handle kills the child and unlinks it (and with it the PTYs) on drop.
fn spawn_terminald() -> DaemonProcess {
    let socket_path = scratch_socket("td-e2e");
    let mut command = Command::new(resolve_terminald_binary());
    command.arg("--socket").arg(&socket_path);
    DaemonProcess::spawn(&mut command, socket_path)
}

/// The acceptance test's `horizon-agentd`, spawned through the testkit's
/// definition of that daemon's hermetic contract -- the same one agentd's
/// own suite spawns through, so this suite cannot silently fall behind it
/// (it used to keep a hand-written copy, which could go stale while staying
/// green against a *less* isolated daemon).
///
/// The binary is resolved as a *sibling* of the terminald one:
/// `CARGO_BIN_EXE_<name>` is only injected for binaries of the package a
/// test belongs to, and every workspace binary is uplifted into the same
/// target directory.
fn spawn_agentd() -> AgentdProcess {
    AgentdSpawn::new(
        sibling_daemon_binary(&resolve_terminald_binary(), "horizon-agent-acp"),
        AgentdPaths::scratch("agentd-td-e2e"),
    )
    .spawn()
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
    connect_hub_client::<TerminalHubClient<WireCodec>>(stream).await
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
    /// Gracefully drains the daemon -- see `drain_with_timeout` for why the
    /// call's own outcome is discarded.
    async fn drain(&self) {
        drain_with_timeout(self.hub.drain()).await;
    }
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
            Ok(Err(error)) if error.is_disconnected() => return,
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

/// The text of the attachment's terminal once it contains `needle` (at
/// once, for an empty one), read as a view reads it: the terminal's state
/// asked for, then the bytes after it taken into a terminal of the test's
/// own.
async fn screen_until(attachment: &mut TerminalAttachment, needle: &str) -> String {
    send_terminal_command(&attachment.commands, TerminalCommand::WantState).await;
    let deadline = tokio::time::Instant::now() + TERMINAL_UPDATE_TIMEOUT;
    let (mut state, mut terminal) = (Vec::new(), None::<TerminalCore>);
    loop {
        if let Some(terminal) = &terminal {
            let text = terminal.snapshot_frame().text();
            if text.contains(needle) {
                return text;
            }
        }
        let update = match tokio::time::timeout_at(deadline, attachment.events.recv()).await {
            Ok(Ok(Some(update))) => update,
            Ok(Ok(None)) => panic!("the events closed before {needle:?}"),
            Ok(Err(error)) => panic!("the events failed before {needle:?}: {error}"),
            Err(_elapsed) => panic!(
                "gave up waiting for {needle:?}; last screen: {:?}",
                terminal
                    .as_ref()
                    .map(|terminal| terminal.snapshot_frame().text())
            ),
        };
        match update {
            TerminalUpdate::State { bytes, more } => {
                state.extend_from_slice(&bytes);
                if !more {
                    terminal = TerminalCore::from_state(&state);
                    state.clear();
                }
            }
            TerminalUpdate::Output(bytes) => {
                if let Some(terminal) = terminal.as_mut() {
                    terminal.take_in(&bytes);
                }
            }
            TerminalUpdate::Resized(size) => {
                if let Some(terminal) = terminal.as_mut() {
                    terminal.resize(size);
                }
            }
            TerminalUpdate::HoldReleased => {
                if let Some(terminal) = terminal.as_mut() {
                    terminal.release_hold();
                }
            }
            _ => {}
        }
    }
}

// --- tests -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_negotiates_reports_the_binary_id_and_drains_over_the_real_socket() {
    let mut terminald = spawn_terminald();
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
    let mut terminald = spawn_terminald();
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
    let terminald = spawn_terminald();
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
    screen_until(&mut attachment, "HORIZON_DIFF_MARKER").await;

    // Disconnect this client entirely; the terminal session keeps running
    // (process-scoped), so a fresh connection can reattach.
    drop(attachment);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
    let mut attachment = client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("attach on a fresh connection should succeed");
    // The state a reattached client asks for already has the marker.
    let attached = screen_until(&mut attachment, "").await;
    assert!(
        attached.contains("HORIZON_DIFF_MARKER"),
        "the state must have what was written before, got: {attached:?}"
    );

    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'HORIZON_REATTACH_MARKER\\n'\n".to_vec()),
    )
    .await;
    screen_until(&mut attachment, "HORIZON_REATTACH_MARKER").await;

    send_terminal_command(&attachment.commands, TerminalCommand::Shutdown).await;
    wait_for_session_end(&mut attachment.events).await;
}

/// `hand_over` gives the terminals to the daemon's next image
/// (`docs/terminald-handover-design.md`): the call is never answered, the
/// same process goes on with the listening socket, and a client that
/// connects again finds each terminal with its screen and the same shell
/// in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handover_keeps_the_terminals_their_screens_and_their_programs() {
    let mut terminald = spawn_terminald();
    let pid = terminald.child.id();
    let session_id = uuid::Uuid::new_v4();
    let client = connect_hub(&terminald.socket_path).await;
    let mut attachment = client
        .hub
        .create_terminal(session_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .expect("create should succeed");
    // (The words are put together by the shell, so what is typed does
    // not read as what it prints.)
    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"KEPT=kept-$((3*3)); printf 'BEFORE_%s\\n' HANDOVER\n".to_vec()),
    )
    .await;
    screen_until(&mut attachment, "BEFORE_HANDOVER").await;

    let answered = client.hub.hand_over().await;
    assert!(
        matches!(answered, Err(HubError::Call(_))),
        "a handover is never answered, got {answered:?}"
    );
    drop(attachment);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
    assert_eq!(
        client.hub.list_terminals().await.unwrap(),
        vec![TerminalSummary { session_id }]
    );
    let mut attachment = client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("the terminal is there after the handover");
    let screen = screen_until(&mut attachment, "BEFORE_HANDOVER").await;
    assert!(screen.contains("BEFORE_HANDOVER"), "{screen:?}");
    send_terminal_command(
        &attachment.commands,
        TerminalCommand::Input(b"printf 'AFTER_%s\\n' \"$KEPT\"\n".to_vec()),
    )
    .await;
    screen_until(&mut attachment, "AFTER_kept-9").await;
    // The process that started it, still running.
    assert_eq!(terminald.child.id(), pid);
    assert!(terminald.child.try_wait().unwrap().is_none());

    send_terminal_command(&attachment.commands, TerminalCommand::Shutdown).await;
    wait_for_session_end(&mut attachment.events).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_list_is_sorted_and_a_missing_attach_is_explicit() {
    let terminald = spawn_terminald();
    let high_id = uuid::Uuid::from_u128(2);
    let low_id = uuid::Uuid::from_u128(1);

    let client = connect_hub(&terminald.socket_path).await;
    assert_eq!(client.hub.list_terminals().await.unwrap(), Vec::new());

    // Create two terminals across two connections; both survive the
    // disconnect (process-scoped sessions).
    let mut high = client
        .hub
        .create_terminal(high_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    // The attachment answers with the terminal's state.
    let _ = screen_until(&mut high, "").await;
    drop(high);
    drop(client);

    let client = connect_hub(&terminald.socket_path).await;
    let mut low = client
        .hub
        .create_terminal(low_id, terminal_spec(std::env::temp_dir(), None))
        .await
        .unwrap();
    let _ = screen_until(&mut low, "").await;
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
    let terminald = spawn_terminald();
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
    let _ = screen_until(&mut source, &source_needle).await;

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
    let _ = screen_until(&mut target, &target_needle).await;

    send_terminal_command(&source.commands, TerminalCommand::Shutdown).await;
    send_terminal_command(&target.commands, TerminalCommand::Shutdown).await;
    std::fs::remove_dir_all(root).unwrap();
}

/// **The terminald split's acceptance property**
/// (`docs/terminald-split-design.md` decisions 1-2): the exact sequence
/// `Reload Agent Runtime` performs against the agent runtime — close its
/// connection, wait for the process to exit, start a fresh one on the same
/// session logs — leaves a `horizon-terminald` session fully usable: still listed,
/// still attachable, its retained frame intact, and its shell still alive
/// (proven by making the same PTY echo a *new* marker afterwards).
///
/// Before the split this was impossible by construction: one process owned
/// both, and its drain killed every PTY. The test is deliberately end-to-end over
/// two real daemons on two real sockets, because that separation is the
/// whole deliverable — the client-side half is pinned separately in
/// `src/runtime/tests.rs`
/// (`draining_the_agent_runtime_leaves_the_terminal_runtime_untouched`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agentd_drain_and_respawn_leaves_a_live_terminald_session_attachable() {
    let terminald = spawn_terminald();
    let mut agentd = spawn_agentd();

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
    let _ = screen_until(&mut attachment, "BEFORE_AGENT_RELOAD").await;

    // `Reload Agent Runtime`, faithfully: close the agent runtime's
    // connection, confirm the process is gone, then start a fresh one on the
    // same session logs.
    drop(agentd.connect().await);
    let status = wait_for_exit(&mut agentd.child).await;
    assert_eq!(
        status.code(),
        Some(0),
        "the agent runtime should exit 0 once its client closes, got {status:?}"
    );
    let mut agentd = agentd.respawn_at_same_paths();
    // The replacement really answers, so the reload sequence is complete,
    // not merely started.
    drop(agentd.connect().await);

    // The terminal daemon never noticed. Its session is still listed...
    assert_eq!(
        terminal_client.hub.list_terminals().await.unwrap(),
        vec![TerminalSummary { session_id }],
        "the agent daemon's restart must not remove a terminal session"
    );
    // ...the existing attachment still carries the pre-reload frame...
    assert!(
        screen_until(&mut attachment, "")
            .await
            .contains("BEFORE_AGENT_RELOAD"),
        "the retained frame must survive the agent daemon's restart"
    );
    // ...a *fresh* attachment reseeds it (the UI-restart path)...
    let mut reattached = terminal_client
        .hub
        .attach_terminal(session_id)
        .await
        .expect("the session must still be attachable after the agent reload");
    assert!(
        screen_until(&mut reattached, "")
            .await
            .contains("BEFORE_AGENT_RELOAD"),
        "a re-attach after the agent reload must reseed the retained frame"
    );
    // ...and the shell itself is still alive, which is the part that
    // actually mattered to the operator: it answers new input.
    send_terminal_command(
        &reattached.commands,
        TerminalCommand::Input(b"printf 'AFTER_AGENT_RELOAD\\n'\n".to_vec()),
    )
    .await;
    let after = screen_until(&mut reattached, "AFTER_AGENT_RELOAD").await;
    assert!(
        after.contains("AFTER_AGENT_RELOAD"),
        "the PTY's shell must still be running after the agent daemon restart"
    );

    send_terminal_command(&reattached.commands, TerminalCommand::Shutdown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undersized_geometry_reaches_the_pty_as_the_same_effective_size() {
    let terminald = spawn_terminald();
    let client = connect_hub(&terminald.socket_path).await;
    let mut spec = terminal_spec(std::env::temp_dir(), None);
    spec.initial_size = TerminalSize::new(0, 0);
    let mut attachment = client
        .hub
        .create_terminal(uuid::Uuid::new_v4(), spec)
        .await
        .unwrap();

    for (step, (resize, expected)) in [
        (None, "1 2"),
        (Some(TerminalSize::new(1, 0)), "1 2"),
        (Some(TerminalSize::new(30, 5)), "5 30"),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(size) = resize {
            send_terminal_command(&attachment.commands, TerminalCommand::Resize(size)).await;
        }
        // OSC title delivery remains observable even on a two-cell viewport.
        // stty reads the real PTY's geometry, independently of the core frame.
        let command = format!("printf '\\033]0;size-{step}:%s\\007' \"$(stty size)\"\n");
        send_terminal_command(
            &attachment.commands,
            TerminalCommand::Input(command.into_bytes()),
        )
        .await;
        let expected = format!("size-{step}:{expected}");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let event = attachment
                    .events
                    .recv()
                    .await
                    .unwrap()
                    .expect("live terminal");
                if let TerminalUpdate::Title(Some(title)) = event
                    && title == expected
                {
                    break;
                }
            }
        })
        .await
        .expect("PTY geometry title should arrive through the core");
    }
    send_terminal_command(&attachment.commands, TerminalCommand::Shutdown).await;
    wait_for_session_end(&mut attachment.events).await;
}
