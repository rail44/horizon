//! Client-runtime tests against an in-process fake `SessionHub` daemon,
//! served over the same remoc stack production uses (`Connect::io` +
//! `SessionHubServerShared`, Postbag codec). The fake hub records every
//! call (handing the test the *peer* halves of each attachment's channels,
//! so tests drive updates and observe commands), which replaces the JSONL
//! era's envelope scripting. Adoption condition 3 note: the client half of
//! every stream here is polled by the runtime's own dedicated thread
//! (`spawn`/`spawn_test_stream`), so serving the fake daemon from the
//! test's runtime is already "both ends concurrently". The tests use a
//! multi-thread flavor because the fake daemon's mux/serve tasks live on
//! the test's own runtime, and the test bodies block it (crossbeam
//! `recv_timeout`, thread joins) exactly like the production sync world
//! does -- on a current-thread runtime that would freeze the daemon.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use horizon_agent::contract::{Event, ProviderId, SessionId, SessionState};
use horizon_agent::wire::{AgentWireEvent, SessionNew, WorkspaceRootResolved};
use horizon_session_protocol::{
    AgentAttachment, ClientHello, HubError, HubHello, SessionHub, SessionHubClient,
    SessionHubServerShared, TerminalAttachment, VersionRange, WireCodec, SESSION_PROTOCOL_VERSION,
};
use horizon_terminal_core::{TerminalColorScheme, TerminalFrame, TerminalSize};
use remoc::rch;
use remoc::rtc::ServerShared as _;
use tokio::task::JoinHandle;

use super::*;

fn spec() -> TerminalSpawnSpec {
    TerminalSpawnSpec {
        shell: "/bin/sh".into(),
        args: Vec::new(),
        term: "xterm-256color".into(),
        scrollback_lines: 1_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: "/tmp/horizon-control.sock".into(),
        fallback_cwd: "/tmp".into(),
        spawn_source_session_id: None,
        initial_size: TerminalSize::new(80, 24),
    }
}

/// The peer halves of a terminal attachment the fake hub handed out: the
/// test sends updates and reads commands through these.
struct TerminalPeer {
    updates: rch::mpsc::Sender<TerminalUpdate, WireCodec>,
    commands: rch::mpsc::Receiver<TerminalCommand, WireCodec>,
}

/// The peer halves of an agent attachment.
struct AgentPeer {
    events: rch::mpsc::Sender<AgentWireEvent, WireCodec>,
    #[allow(dead_code)]
    commands: rch::mpsc::Receiver<Command, WireCodec>,
}

/// One recorded hub call, with whatever live halves the fake daemon kept.
enum FakeCall {
    Hello,
    CreateTerminal {
        session_id: Uuid,
        spec: TerminalSpawnSpec,
        peer: TerminalPeer,
    },
    AttachTerminal {
        session_id: Uuid,
        /// `None` when the fake reported `TerminalNotFound`.
        peer: Option<TerminalPeer>,
    },
    NewAgent {
        new: SessionNew,
        peer: AgentPeer,
    },
    AttachAgent {
        session_id: SessionId,
        peer: AgentPeer,
    },
    ListTerminals,
    ListAgents,
    Drain,
}

impl std::fmt::Debug for FakeCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            FakeCall::Hello => "Hello",
            FakeCall::CreateTerminal { .. } => "CreateTerminal",
            FakeCall::AttachTerminal { .. } => "AttachTerminal",
            FakeCall::NewAgent { .. } => "NewAgent",
            FakeCall::AttachAgent { .. } => "AttachAgent",
            FakeCall::ListTerminals => "ListTerminals",
            FakeCall::ListAgents => "ListAgents",
            FakeCall::Drain => "Drain",
        };
        f.write_str(name)
    }
}

/// Scripted behavior for the fake hub.
#[derive(Default)]
struct FakeBehavior {
    /// Reject `hello` with a version-range error.
    reject_hello: bool,
    /// Ids `attach_terminal` reports `TerminalNotFound` for.
    missing_terminals: Vec<Uuid>,
    /// Successive `list_terminals` replies, popped front-first; empty →
    /// reply with an empty list.
    terminal_lists: Vec<Vec<TerminalSummary>>,
}

struct FakeHub {
    behavior: StdMutex<FakeBehavior>,
    calls: tokio::sync::mpsc::UnboundedSender<FakeCall>,
}

impl FakeHub {
    fn terminal_attachment(&self) -> (TerminalAttachment, TerminalPeer) {
        let (update_tx, update_rx) = rch::mpsc::channel::<TerminalUpdate, WireCodec>(16);
        let (command_tx, command_rx) = rch::mpsc::channel::<TerminalCommand, WireCodec>(16);
        (
            TerminalAttachment {
                updates: update_rx,
                commands: command_tx,
            },
            TerminalPeer {
                updates: update_tx,
                commands: command_rx,
            },
        )
    }

    fn agent_attachment(&self) -> (AgentAttachment, AgentPeer) {
        let (event_tx, event_rx) = rch::mpsc::channel::<AgentWireEvent, WireCodec>(16);
        let (command_tx, command_rx) = rch::mpsc::channel::<Command, WireCodec>(16);
        (
            AgentAttachment {
                events: event_rx,
                commands: command_tx,
            },
            AgentPeer {
                events: event_tx,
                commands: command_rx,
            },
        )
    }
}

impl SessionHub for FakeHub {
    async fn hello(&self, _client: ClientHello) -> Result<HubHello, HubError> {
        if self.behavior.lock().unwrap().reject_hello {
            return Err(HubError::IncompatibleVersion {
                client: VersionRange::ours(),
                daemon: VersionRange {
                    min_supported: SESSION_PROTOCOL_VERSION + 5,
                    current: SESSION_PROTOCOL_VERSION + 5,
                },
            });
        }
        let _ = self.calls.send(FakeCall::Hello);
        let (_request_tx, request_rx) = rch::mpsc::channel::<HostToolRequest, WireCodec>(4);
        let (response_tx, _response_rx) = rch::mpsc::channel::<HostToolResponse, WireCodec>(4);
        let (_skipped_tx, skipped_rx) = rch::mpsc::channel::<String, WireCodec>(1);
        Ok(HubHello {
            negotiated: SESSION_PROTOCOL_VERSION,
            binary_id: "fake-sessiond".to_string(),
            host_tools: request_rx,
            host_tool_responses: response_tx,
            skipped_lines: skipped_rx,
        })
    }

    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError> {
        let _ = self.calls.send(FakeCall::ListTerminals);
        let mut behavior = self.behavior.lock().unwrap();
        if behavior.terminal_lists.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(behavior.terminal_lists.remove(0))
        }
    }

    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError> {
        let (attachment, peer) = self.terminal_attachment();
        let _ = self.calls.send(FakeCall::CreateTerminal {
            session_id,
            spec,
            peer,
        });
        Ok(attachment)
    }

    async fn attach_terminal(&self, session_id: Uuid) -> Result<TerminalAttachment, HubError> {
        if self
            .behavior
            .lock()
            .unwrap()
            .missing_terminals
            .contains(&session_id)
        {
            let _ = self.calls.send(FakeCall::AttachTerminal {
                session_id,
                peer: None,
            });
            return Err(HubError::TerminalNotFound);
        }
        let (attachment, peer) = self.terminal_attachment();
        let _ = self.calls.send(FakeCall::AttachTerminal {
            session_id,
            peer: Some(peer),
        });
        Ok(attachment)
    }

    async fn list_agents(&self) -> Result<Vec<wire::SessionSummary>, HubError> {
        let _ = self.calls.send(FakeCall::ListAgents);
        Ok(Vec::new())
    }

    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError> {
        let (attachment, peer) = self.agent_attachment();
        let _ = self.calls.send(FakeCall::NewAgent { new, peer });
        Ok(attachment)
    }

    async fn attach_agent(&self, session_id: SessionId) -> Result<AgentAttachment, HubError> {
        let (attachment, peer) = self.agent_attachment();
        let _ = self.calls.send(FakeCall::AttachAgent { session_id, peer });
        Ok(attachment)
    }

    async fn drain(&self) -> Result<(), HubError> {
        let _ = self.calls.send(FakeCall::Drain);
        Ok(())
    }
}

/// Serves a [`FakeHub`] over `stream`. Returns the recorded-call receiver
/// plus the serve/mux task handles (abort them to simulate the daemon
/// dying).
async fn serve_fake_hub<S>(
    stream: S,
    behavior: FakeBehavior,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<FakeCall>,
    JoinHandle<()>,
    JoinHandle<()>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let (conn, mut base_tx, _base_rx) =
        remoc::Connect::io::<_, _, SessionHubClient<WireCodec>, (), WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        )
        .await
        .expect("fake daemon remoc connect");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let (calls_tx, calls_rx) = tokio::sync::mpsc::unbounded_channel();
    let hub = FakeHub {
        behavior: StdMutex::new(behavior),
        calls: calls_tx,
    };
    let (server, client) = SessionHubServerShared::<_, WireCodec>::new(std::sync::Arc::new(hub), 8);
    base_tx
        .send(client)
        .await
        .expect("hand the hub client to the runtime");
    let serve_task = tokio::spawn(async move {
        let _ = server.serve(true).await;
    });
    (calls_rx, conn_task, serve_task)
}

async fn next_call(calls: &mut tokio::sync::mpsc::UnboundedReceiver<FakeCall>) -> FakeCall {
    tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .expect("timed out waiting for a hub call")
        .expect("fake daemon stopped recording calls")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_returns_before_the_connection_and_a_queued_create_arrives_after() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let started = Instant::now();
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    assert!(started.elapsed() < Duration::from_millis(50));

    // Queued before the daemon has even completed a handshake.
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());

    let (mut calls, _conn, _serve) = serve_fake_hub(server, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    let FakeCall::CreateTerminal {
        session_id,
        spec: received_spec,
        peer,
    } = next_call(&mut calls).await
    else {
        panic!("expected the queued create to arrive first after the handshake");
    };
    assert_eq!(session_id, terminal_id);
    assert_eq!(received_spec, spec());

    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("ready".into()));
    peer.updates.send(snapshot.clone()).await.unwrap();
    assert_eq!(
        terminal
            .updates()
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        snapshot
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_and_terminal_traffic_flows_through_their_own_attachments() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let agent = handle.start_session(agent_id, ProviderId("mock".into()), None, None, None, false);

    let (mut calls, _conn, _serve) = serve_fake_hub(server, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    let mut terminal_peer = None;
    let mut agent_peer = None;
    for _ in 0..2 {
        match next_call(&mut calls).await {
            FakeCall::CreateTerminal {
                session_id, peer, ..
            } => {
                assert_eq!(session_id, terminal_id);
                terminal_peer = Some(peer);
            }
            FakeCall::NewAgent { new, peer } => {
                assert_eq!(new.session_id, agent_id);
                agent_peer = Some(peer);
            }
            other => panic!("unexpected call: {other:?}"),
        }
    }
    let terminal_peer = terminal_peer.expect("terminal create call");
    let agent_peer = agent_peer.expect("agent new call");

    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("terminal".into()));
    terminal_peer.updates.send(snapshot.clone()).await.unwrap();
    let event = Event::StateChanged(SessionState::WaitingForUser);
    agent_peer
        .events
        .send(AgentWireEvent::Event(event.clone()))
        .await
        .unwrap();
    terminal
        .sender()
        .send(TerminalCommand::Input(b"fifo".to_vec()))
        .unwrap();

    assert_eq!(
        terminal
            .updates()
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        snapshot
    );
    assert_eq!(
        agent
            .events()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .event,
        event
    );
    let mut commands = terminal_peer.commands;
    let command = tokio::time::timeout(Duration::from_secs(5), commands.recv())
        .await
        .expect("timed out waiting for the terminal command")
        .unwrap()
        .expect("terminal command");
    assert_eq!(command, TerminalCommand::Input(b"fifo".to_vec()));
}

/// `AgentWireEvent::WorkspaceRootResolved` is a live daemon->shell
/// announcement (`docs/session-relationship-design.md`'s "still eventual,
/// not live" gap), not a `contract::ProviderEvent` any per-session route
/// folds -- `Routes` sends it on its own `workspace_roots` channel instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incoming_workspace_root_resolved_reaches_its_own_channel() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, workspace_roots) = SessiondHandle::start_on_stream(client);
    let session_id = SessionId::new();
    let _agent = handle.attach_session(session_id);

    let (mut calls, _conn, _serve) = serve_fake_hub(server, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    let FakeCall::AttachAgent {
        session_id: attached_id,
        peer,
    } = next_call(&mut calls).await
    else {
        panic!("expected the attach call");
    };
    assert_eq!(attached_id, session_id);

    let parent_id = SessionId::new();
    let resolved = WorkspaceRootResolved {
        workspace_root: std::path::PathBuf::from("/tmp/repo/.horizon/worktrees/abcd1234"),
        parent_session_id: Some(parent_id),
    };
    peer.events
        .send(AgentWireEvent::WorkspaceRootResolved(resolved.clone()))
        .await
        .unwrap();

    let (received_session_id, received_resolved) = workspace_roots
        .recv_timeout(Duration::from_secs(5))
        .expect("the WorkspaceRootResolved event should reach its own channel");
    assert_eq!(received_session_id, session_id);
    assert_eq!(received_resolved, resolved);
}

/// The JSONL wire needed a `request_id` correlation map to keep two
/// in-flight terminal lists apart; rtc calls return futures, so the reply
/// routing is structural now. Two concurrent lists must still each get
/// their own answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_terminal_lists_each_get_their_own_reply() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);

    let first_session = Uuid::new_v4();
    let second_session = Uuid::new_v4();
    let behavior = FakeBehavior {
        terminal_lists: vec![
            vec![TerminalSummary {
                session_id: first_session,
            }],
            vec![TerminalSummary {
                session_id: second_session,
            }],
        ],
        ..FakeBehavior::default()
    };
    let (_calls, _conn, _serve) = serve_fake_hub(server, behavior).await;

    let first_handle = handle.clone();
    let first = std::thread::spawn(move || first_handle.terminal_list().unwrap());
    let second_handle = handle.clone();
    let second = std::thread::spawn(move || second_handle.terminal_list().unwrap());

    let mut returned = vec![
        first.join().unwrap()[0].session_id,
        second.join().unwrap()[0].session_id,
    ];
    returned.sort_unstable();
    let mut expected = vec![first_session, second_session];
    expected.sort_unstable();
    assert_eq!(returned, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_batch_attach_keeps_attached_sessions_and_drops_not_found() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let attached_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();

    let behavior = FakeBehavior {
        missing_terminals: vec![missing_id],
        ..FakeBehavior::default()
    };
    let (mut calls, _conn, _serve) = serve_fake_hub(server, behavior).await;

    let attach_handle = handle.clone();
    let attached =
        std::thread::spawn(move || attach_handle.attach_terminals(vec![attached_id, missing_id]));

    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    let mut peers = HashMap::new();
    for _ in 0..2 {
        let FakeCall::AttachTerminal { session_id, peer } = next_call(&mut calls).await else {
            panic!("expected an attach call");
        };
        peers.insert(session_id, peer);
    }
    assert!(peers[&missing_id].is_none());
    let peer = peers
        .remove(&attached_id)
        .flatten()
        .expect("the attached session should have live channels");

    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("survived".into()));
    peer.updates.send(snapshot.clone()).await.unwrap();

    let mut sessions = attached.join().unwrap();
    assert_eq!(sessions.len(), 1);
    let (session_id, session) = sessions.pop().unwrap();
    assert_eq!(session_id, attached_id);
    assert_eq!(
        session
            .updates()
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        snapshot
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_runtime_does_not_send_drain() {
    let (client, server) = tokio::io::duplex(4096);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let responder = handle.responder();
    let (mut calls, _conn, serve) = serve_fake_hub(server, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));

    drop(handle);

    // The serve loop ends because the client went away -- and the call log
    // closes without ever recording a Drain.
    tokio::time::timeout(Duration::from_secs(5), serve)
        .await
        .expect("the fake daemon's serve loop should end after the runtime drops")
        .unwrap();
    let mut saw = Vec::new();
    while let Ok(call) = calls.try_recv() {
        saw.push(call);
    }
    assert!(
        !saw.iter().any(|call| matches!(call, FakeCall::Drain)),
        "dropping the runtime must not drain the daemon: {saw:?}"
    );
    drop(responder);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_before_the_daemon_answers_cancels_the_runtime() {
    let (client, _server) = tokio::io::duplex(4096);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    // Nothing serves the daemon side, so the runtime is still trying to
    // establish; stop_and_wait must cancel that and return promptly.
    let stopped = std::thread::spawn(move || handle.stop_and_wait());
    tokio::task::spawn_blocking(move || stopped.join().unwrap())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn established_disconnect_reports_errors_without_reconnecting() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let agent = handle.start_session(agent_id, ProviderId("mock".into()), None, None, None, false);

    let (mut calls, conn, serve) = serve_fake_hub(server, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    for _ in 0..2 {
        next_call(&mut calls).await;
    }

    // The daemon dies: mux and serve loop torn down.
    conn.abort();
    serve.abort();

    let terminal_error = terminal
        .updates()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(matches!(terminal_error, TerminalUpdate::Error(_)));
    let agent_error = agent.events().recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(agent_error.event, Event::Error(_)));

    assert!(handle
        .session_list()
        .unwrap_err()
        .contains("runtime stopped"));
    let late_terminal = handle.start_terminal(Uuid::new_v4(), spec());
    assert!(matches!(
        late_terminal
            .updates()
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        TerminalUpdate::Error(_)
    ));
}

/// A version-range rejection on a test stream (no real socket to drain, no
/// daemon to respawn) must surface as a terminal failure rather than being
/// retried or recovered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_hello_on_a_test_stream_is_a_terminal_failure() {
    let (client, server) = tokio::io::duplex(4096);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (_calls, _conn, _serve) = serve_fake_hub(server, behavior).await;

    let error = handle.session_list().unwrap_err();
    assert!(
        error.contains("runtime stopped"),
        "the runtime should stop after a rejected hello; error was: {error}"
    );
    let late_terminal = handle.start_terminal(Uuid::new_v4(), spec());
    let update = late_terminal
        .updates()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let TerminalUpdate::Error(message) = update else {
        panic!("expected the rejection to fan out as an error, got {update:?}");
    };
    assert!(
        message.contains("rejected the handshake"),
        "error was: {message}"
    );
}

fn stub_socket_paths(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    // Keep well under SUN_LEN, same as the sessiond e2e tests.
    let short_id = &Uuid::new_v4().simple().to_string()[..8];
    (
        std::env::temp_dir().join(format!("hzn-{tag}-{short_id}.sock")),
        std::env::temp_dir().join(format!("hzn-{tag}-ctl-{short_id}.sock")),
    )
}

/// Binds a fresh stub listener at `path`, removing any stale socket file
/// first -- the same stale-file handling the real daemon's `bind_listener`
/// performs, which matters after simulating a drained daemon's exit (its
/// `std::process::exit(0)` leaves the socket file behind).
fn bind_stub_listener(path: &std::path::Path) -> tokio::net::UnixListener {
    let _ = std::fs::remove_file(path);
    tokio::net::UnixListener::bind(path).unwrap()
}

/// Reads everything the client runtime wrote to a JSONL-generation stub
/// until the connection closes, returning the raw bytes.
async fn read_until_closed(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(read)) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    buffer
}

/// The cross-generation recovery loop (`docs/remoc-adoption-design.md` §6,
/// re-anchoring PR #18's scenarios on the new detection path): a v10
/// runtime meets a still-running JSONL daemon — which reads our chmux
/// hello as garbage and closes without a word — detects the generation
/// mismatch via the bounded connect timeout, probes a legacy `Drain` at
/// the newest JSONL version, and adopts the respawned (remoc) daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jsonl_generation_daemon_is_probed_drained_and_the_respawn_adopted() {
    let (socket_path, control_socket) = stub_socket_paths("probe");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);

    // Connection 1: the JSONL daemon reads a chunk of chmux bytes it can't
    // parse and slams the connection, exactly like read_envelope's error
    // arm did.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::AsyncReadExt;
        let mut chunk = [0_u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("the runtime should send its chmux hello");
        drop(stream);
    }

    // Connection 2: the first drain probe — one newline-terminated JSONL
    // envelope, stamped with the newest JSONL version.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        let bytes = read_until_closed(&mut stream).await;
        let line = String::from_utf8(bytes).expect("the drain probe is one JSON line");
        assert_eq!(
            line,
            horizon_session_protocol::legacy::drain_line(
                horizon_session_protocol::legacy::NEWEST_JSONL_VERSION
            ),
            "the first probe must be aimed at the newest JSONL version"
        );
    }

    // The probe found its mark: "exit" (stop accepting, socket file left
    // behind), then come back as the respawned remoc daemon.
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);

    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn, _serve) = serve_fake_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));

    // Prove the recovered connection is fully established with a round
    // trip.
    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.terminal_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Recovery is attempted exactly once per runtime: if the replacement
/// daemon still can't speak remoc (a stale horizon-sessiond binary that a
/// rebuild never touched), the runtime must fail loudly instead of
/// drain-and-restarting forever, with the rebuild hint in the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_generation_mismatch_after_recovery_goes_fatal_instead_of_looping() {
    let (socket_path, control_socket) = stub_socket_paths("fatal");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);
    let terminal = handle.start_terminal(Uuid::new_v4(), spec());

    // Connection 1: silent JSONL-generation daemon.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::AsyncReadExt;
        let mut chunk = [0_u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("the runtime should send its chmux hello");
        drop(stream);
    }
    // Connection 2: the one drain probe this runtime is allowed.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_until_closed(&mut stream).await;
    }
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);

    // Connection 3: the "respawned" daemon is just as stale.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::AsyncReadExt;
        let mut chunk = [0_u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("the runtime should retry its chmux hello");
        drop(stream);
    }

    // The runtime gives up rather than draining again, with the rebuild
    // hint, fanned out to the registered routes.
    let update = terminal
        .updates()
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    let TerminalUpdate::Error(message) = update else {
        panic!("expected the fatal mismatch to fan out as an error, got {update:?}");
    };
    assert!(
        message.contains("already attempted") && message.contains("rebuild"),
        "error was: {message}"
    );
    let no_more_connections =
        tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
    assert!(
        no_more_connections.is_err(),
        "the runtime must not reconnect (or drain again) after going fatal"
    );

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// A healthy *remoc* daemon whose hub rejects the version range is drained
/// over a fresh hub connection (the rtc successor of the JSONL
/// `HandshakeRejected` recovery) and the respawned daemon is adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_range_rejecting_remoc_daemon_is_drained_via_rtc_and_the_respawn_adopted() {
    let (socket_path, control_socket) = stub_socket_paths("rej");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);

    // Connection 1: hub answers hello with a range rejection.
    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (_calls_1, _conn_1, _serve_1) = serve_fake_hub(stream, behavior).await;

    // Connection 2: the recovery drain arrives as an rtc call.
    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (mut drain_calls, _conn_2, _serve_2) = serve_fake_hub(stream, behavior).await;
    let drain = next_call(&mut drain_calls).await;
    assert!(matches!(drain, FakeCall::Drain), "got {drain:?}");

    // "Exit", then come back as a compatible daemon.
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);
    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn_3, _serve_3) = serve_fake_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.terminal_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Host-side coverage for `SessiondHandle::broadcast_terminal_color_scheme`
/// (the live theme-apply re-push, and its adoption-path use in
/// `spawn_workspace_restore`/`spawn_terminal_resume` -- both call it only
/// after `attach_terminals` returns, exactly as reproduced here): it must
/// inject a `TerminalCommand::SetColorScheme` into every attached
/// session's command stream and nothing for a session `attach_terminals`
/// reported not-found for (whose route is already dropped by the time the
/// broadcast runs, via `TerminalSessionHandle`'s `Drop`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn broadcast_terminal_color_scheme_targets_exactly_the_attached_sessions() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);

    let attached_a = Uuid::new_v4();
    let attached_b = Uuid::new_v4();
    let missing = Uuid::new_v4();
    let behavior = FakeBehavior {
        missing_terminals: vec![missing],
        ..FakeBehavior::default()
    };
    let (mut calls, _conn, _serve) = serve_fake_hub(server, behavior).await;

    let attach_handle = handle.clone();
    let attached = std::thread::spawn(move || {
        attach_handle.attach_terminals(vec![attached_a, attached_b, missing])
    });

    assert!(matches!(next_call(&mut calls).await, FakeCall::Hello));
    let mut peers = HashMap::new();
    for _ in 0..3 {
        let FakeCall::AttachTerminal { session_id, peer } = next_call(&mut calls).await else {
            panic!("expected an attach call");
        };
        peers.insert(session_id, peer);
    }
    let sessions = attached.join().unwrap();
    assert_eq!(
        sessions.len(),
        2,
        "only the two attached results should survive"
    );

    // Mirrors `spawn_workspace_restore`/`spawn_terminal_resume`'s own
    // sequencing: the re-push is sent only after `attach_terminals`
    // returns -- by which point `missing`'s route has already been dropped
    // (`TerminalSessionHandle::drop`, on its not-found result above) and
    // both `attached_a`/`attached_b`'s are confirmed live.
    let scheme = TerminalColorScheme::default();
    handle.broadcast_terminal_color_scheme(scheme);

    for id in [attached_a, attached_b] {
        let mut peer = peers.remove(&id).flatten().expect("attached peer");
        let command = tokio::time::timeout(Duration::from_secs(5), peer.commands.recv())
            .await
            .expect("timed out waiting for the SetColorScheme command")
            .unwrap()
            .expect("SetColorScheme command");
        assert_eq!(command, TerminalCommand::SetColorScheme(scheme));
        // Nothing further follows on this stream.
        let extra = tokio::time::timeout(Duration::from_millis(100), peer.commands.recv()).await;
        assert!(extra.is_err(), "unexpected extra command for {id}");
    }
    // The never-attached session has no peer channels at all (its attach
    // returned an error), which is the structural form of "must not
    // receive a push".
    assert!(peers.remove(&missing).flatten().is_none());
}
