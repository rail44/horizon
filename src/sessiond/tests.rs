//! Client-runtime tests against in-process fake daemons, served over the
//! same remoc stack production uses (`Connect::io` + the rtc
//! `*ServerShared`, Postbag codec). Each fake hub records every call
//! (handing the test the *peer* halves of each attachment's channels, so
//! tests drive updates and observe commands), which replaces the JSONL era's
//! envelope scripting.
//!
//! Since the terminald split there are two of everything: a
//! [`FakeSessionHub`] standing in for `horizon-agentd` and a
//! [`FakeTerminalHub`] for `horizon-terminald`, driven through
//! [`SessiondHandle`] and [`TerminaldHandle`] respectively. The tests that
//! matter most for the split are the ones that run *both* at once and prove
//! they are independent — routing per call site, and a drain of one leaving
//! the other's sessions live.
//!
//! Adoption condition 3 note: the client half of every stream here is polled
//! by the runtime's own dedicated thread (`spawn`/`spawn_test_stream`), so
//! serving the fake daemon from the test's runtime is already "both ends
//! concurrently". The tests use a multi-thread flavor because the fake
//! daemons' mux/serve tasks live on the test's own runtime, and the test
//! bodies block it (crossbeam `recv_timeout`, thread joins) exactly like the
//! production sync world does -- on a current-thread runtime that would
//! freeze the daemon.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use horizon_agent::contract::{Event, ProviderId, SessionId, SessionState};
use horizon_agent::wire::{AgentWireEvent, SessionNew, WorkspaceRootResolved};
use horizon_session_protocol::{
    AgentAttachment, ClientHello, HubError, HubHello, SessionHub, SessionHubClient,
    SessionHubServerShared, TerminalAttachment, TerminalHub, TerminalHubClient, TerminalHubHello,
    TerminalHubServerShared, VersionRange, WireCodec, SESSION_PROTOCOL_VERSION,
};
use horizon_terminal_core::{
    ClipboardDestination, TerminalColorScheme, TerminalFrame, TerminalSize,
};
use remoc::rch;
use remoc::rch::watch::WatchExt as _;
use remoc::rtc::{Client as _, ServerShared as _};
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
/// test publishes frames on the `frames` watch, sends non-frame events on
/// `events`, and reads commands through `commands`.
struct TerminalPeer {
    frames: rch::watch::Sender<TerminalFrame, WireCodec>,
    events: rch::mpsc::Sender<TerminalUpdate, WireCodec>,
    commands: rch::mpsc::Receiver<TerminalCommand, WireCodec>,
}

/// The peer halves of an agent attachment.
struct AgentPeer {
    events: rch::mpsc::Sender<AgentWireEvent, WireCodec>,
    #[allow(dead_code)]
    commands: rch::mpsc::Receiver<Command, WireCodec>,
}

/// One recorded terminal-hub call, with whatever live halves the fake
/// daemon kept.
enum TerminalCall {
    Hello,
    CreateTerminal {
        session_id: Uuid,
        // Both boxed for `clippy::large_enum_variant`: `TerminalPeer`
        // carries a watch sender, and `TerminalSpawnSpec` is a wide struct
        // -- together they dwarf every other recorded call.
        spec: Box<TerminalSpawnSpec>,
        peer: Box<TerminalPeer>,
    },
    AttachTerminal {
        session_id: Uuid,
        /// `None` when the fake reported `TerminalNotFound`.
        peer: Option<Box<TerminalPeer>>,
    },
    ListTerminals,
    Drain,
}

/// One recorded agent-hub call.
enum AgentCall {
    Hello,
    NewAgent {
        new: SessionNew,
        peer: AgentPeer,
    },
    AttachAgent {
        session_id: SessionId,
        peer: AgentPeer,
    },
    ListAgents,
    Drain,
}

impl std::fmt::Debug for TerminalCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            TerminalCall::Hello => "Hello",
            TerminalCall::CreateTerminal { .. } => "CreateTerminal",
            TerminalCall::AttachTerminal { .. } => "AttachTerminal",
            TerminalCall::ListTerminals => "ListTerminals",
            TerminalCall::Drain => "Drain",
        };
        f.write_str(name)
    }
}

impl std::fmt::Debug for AgentCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            AgentCall::Hello => "Hello",
            AgentCall::NewAgent { .. } => "NewAgent",
            AgentCall::AttachAgent { .. } => "AttachAgent",
            AgentCall::ListAgents => "ListAgents",
            AgentCall::Drain => "Drain",
        };
        f.write_str(name)
    }
}

/// Scripted behavior shared by both fake hubs.
#[derive(Default)]
struct FakeBehavior {
    /// Reject `hello` with a version-range error.
    reject_hello: bool,
    /// Never answer `hello` (the call blocks forever) — for the
    /// drop-during-hello test, which aborts the daemon while the call is
    /// in flight.
    hang_hello: bool,
    /// Answer `hello` normally but fail every later call — the terminald
    /// skew insurance's trigger (`docs/terminald-split-design.md` decision
    /// 6): a peer that negotiates fine and then cannot actually be talked
    /// to.
    fail_after_hello: bool,
    /// Ids `attach_terminal` reports `TerminalNotFound` for.
    missing_terminals: Vec<Uuid>,
    /// Successive `list_terminals` replies, popped front-first; empty →
    /// reply with an empty list.
    terminal_lists: Vec<Vec<TerminalSummary>>,
    /// The frame `attach_terminal` seeds its watch with — the retained
    /// latest frame a real reattach reseeds. `None` → an empty seed.
    attach_seed: Option<TerminalFrame>,
}

struct FakeTerminalHub {
    behavior: StdMutex<FakeBehavior>,
    calls: tokio::sync::mpsc::UnboundedSender<TerminalCall>,
}

struct FakeSessionHub {
    behavior: StdMutex<FakeBehavior>,
    calls: tokio::sync::mpsc::UnboundedSender<AgentCall>,
}

impl FakeTerminalHub {
    fn terminal_attachment(&self, seed: TerminalFrame) -> (TerminalAttachment, TerminalPeer) {
        let (frame_tx, frame_rx) = rch::watch::channel::<TerminalFrame, WireCodec>(seed)
            .with_max_item_size::<{ horizon_session_protocol::FRAME_MAX_ITEM_BYTES }>();
        let (event_tx, event_rx) = rch::mpsc::channel::<TerminalUpdate, WireCodec>(16);
        let event_rx = event_rx
            .set_max_item_size::<{ horizon_session_protocol::TERMINAL_EVENT_MAX_ITEM_BYTES }>();
        let (command_tx, command_rx) = rch::mpsc::channel::<TerminalCommand, WireCodec>(16);
        (
            TerminalAttachment {
                frames: frame_rx,
                events: event_rx,
                commands: command_tx,
            },
            TerminalPeer {
                frames: frame_tx,
                events: event_tx,
                commands: command_rx,
            },
        )
    }
}

impl FakeSessionHub {
    fn agent_attachment(&self) -> (AgentAttachment, AgentPeer) {
        let (event_tx, event_rx) = rch::mpsc::channel::<AgentWireEvent, WireCodec>(16);
        let event_rx =
            event_rx.set_max_item_size::<{ horizon_session_protocol::TOOL_IO_MAX_ITEM_BYTES }>();
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

fn rejected_hello() -> HubError {
    HubError::IncompatibleVersion {
        client: VersionRange::ours(),
        daemon: VersionRange {
            min_supported: SESSION_PROTOCOL_VERSION + 5,
            current: SESSION_PROTOCOL_VERSION + 5,
        },
    }
}

impl TerminalHub for FakeTerminalHub {
    async fn hello(&self, _client: ClientHello) -> Result<TerminalHubHello, HubError> {
        if self.behavior.lock().unwrap().hang_hello {
            std::future::pending::<()>().await;
        }
        if self.behavior.lock().unwrap().reject_hello {
            return Err(rejected_hello());
        }
        let _ = self.calls.send(TerminalCall::Hello);
        Ok(TerminalHubHello {
            negotiated: SESSION_PROTOCOL_VERSION,
            binary_id: "fake-terminald".to_string(),
        })
    }

    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError> {
        let _ = self.calls.send(TerminalCall::ListTerminals);
        let mut behavior = self.behavior.lock().unwrap();
        if behavior.fail_after_hello {
            return Err(HubError::Unknown);
        }
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
        let (attachment, peer) = self.terminal_attachment(TerminalFrame::empty());
        let _ = self.calls.send(TerminalCall::CreateTerminal {
            session_id,
            spec: Box::new(spec),
            peer: Box::new(peer),
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
            let _ = self.calls.send(TerminalCall::AttachTerminal {
                session_id,
                peer: None,
            });
            return Err(HubError::TerminalNotFound);
        }
        let seed = self
            .behavior
            .lock()
            .unwrap()
            .attach_seed
            .clone()
            .unwrap_or_else(TerminalFrame::empty);
        let (attachment, peer) = self.terminal_attachment(seed);
        let _ = self.calls.send(TerminalCall::AttachTerminal {
            session_id,
            peer: Some(Box::new(peer)),
        });
        Ok(attachment)
    }

    async fn drain(&self) -> Result<(), HubError> {
        let _ = self.calls.send(TerminalCall::Drain);
        Ok(())
    }
}

impl SessionHub for FakeSessionHub {
    async fn hello(&self, _client: ClientHello) -> Result<HubHello, HubError> {
        if self.behavior.lock().unwrap().hang_hello {
            std::future::pending::<()>().await;
        }
        if self.behavior.lock().unwrap().reject_hello {
            return Err(rejected_hello());
        }
        let _ = self.calls.send(AgentCall::Hello);
        let (_request_tx, request_rx) = rch::mpsc::channel::<HostToolRequest, WireCodec>(4);
        let request_rx =
            request_rx.set_max_item_size::<{ horizon_session_protocol::TOOL_IO_MAX_ITEM_BYTES }>();
        let (response_tx, _response_rx) = rch::mpsc::channel::<HostToolResponse, WireCodec>(4);
        let (_skipped_tx, skipped_rx) = rch::mpsc::channel::<String, WireCodec>(1);
        let skipped_rx =
            skipped_rx.set_max_item_size::<{ horizon_session_protocol::CONTROL_MAX_ITEM_BYTES }>();
        Ok(HubHello {
            negotiated: SESSION_PROTOCOL_VERSION,
            binary_id: "fake-agentd".to_string(),
            host_tools: request_rx,
            host_tool_responses: response_tx,
            skipped_lines: skipped_rx,
        })
    }

    async fn list_agents(&self) -> Result<Vec<wire::SessionSummary>, HubError> {
        let _ = self.calls.send(AgentCall::ListAgents);
        Ok(Vec::new())
    }

    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError> {
        let (attachment, peer) = self.agent_attachment();
        let _ = self.calls.send(AgentCall::NewAgent { new, peer });
        Ok(attachment)
    }

    async fn attach_agent(&self, session_id: SessionId) -> Result<AgentAttachment, HubError> {
        let (attachment, peer) = self.agent_attachment();
        let _ = self.calls.send(AgentCall::AttachAgent { session_id, peer });
        Ok(attachment)
    }

    async fn drain(&self) -> Result<(), HubError> {
        let _ = self.calls.send(AgentCall::Drain);
        Ok(())
    }

    async fn reload_provider_config(&self) -> Result<(), HubError> {
        Ok(())
    }
}

/// Serves a [`FakeTerminalHub`] over `stream`. Returns the recorded-call
/// receiver plus the serve/mux task handles (abort them to simulate the
/// daemon dying).
async fn serve_fake_terminal_hub<S>(
    stream: S,
    behavior: FakeBehavior,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<TerminalCall>,
    JoinHandle<()>,
    JoinHandle<()>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let (conn, mut base_tx, _base_rx) =
        remoc::Connect::io::<_, _, TerminalHubClient<WireCodec>, (), WireCodec>(
            remoc::Cfg::default(),
            read_half,
            write_half,
        )
        .await
        .expect("fake terminald remoc connect");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let (calls_tx, calls_rx) = tokio::sync::mpsc::unbounded_channel();
    let hub = FakeTerminalHub {
        behavior: StdMutex::new(behavior),
        calls: calls_tx,
    };
    let (server, mut client) =
        TerminalHubServerShared::<_, WireCodec>::new(std::sync::Arc::new(hub), 8);
    // Mirror the real daemon's pre-transport rtc caps (main.rs) so the
    // boundary tests exercise the same enforcement.
    client.set_max_request_size(horizon_session_protocol::RTC_MAX_REQUEST_BYTES);
    client.set_max_reply_size(horizon_session_protocol::RTC_MAX_REPLY_BYTES);
    base_tx
        .send(client)
        .await
        .expect("hand the hub client to the runtime");
    let serve_task = tokio::spawn(async move {
        let _ = server.serve(true).await;
    });
    (calls_rx, conn_task, serve_task)
}

/// [`serve_fake_terminal_hub`]'s agent-daemon twin.
async fn serve_fake_session_hub<S>(
    stream: S,
    behavior: FakeBehavior,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<AgentCall>,
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
        .expect("fake agentd remoc connect");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let (calls_tx, calls_rx) = tokio::sync::mpsc::unbounded_channel();
    let hub = FakeSessionHub {
        behavior: StdMutex::new(behavior),
        calls: calls_tx,
    };
    let (server, mut client) =
        SessionHubServerShared::<_, WireCodec>::new(std::sync::Arc::new(hub), 8);
    client.set_max_request_size(horizon_session_protocol::RTC_MAX_REQUEST_BYTES);
    client.set_max_reply_size(horizon_session_protocol::RTC_MAX_REPLY_BYTES);
    base_tx
        .send(client)
        .await
        .expect("hand the hub client to the runtime");
    let serve_task = tokio::spawn(async move {
        let _ = server.serve(true).await;
    });
    (calls_rx, conn_task, serve_task)
}

async fn next_terminal_call(
    calls: &mut tokio::sync::mpsc::UnboundedReceiver<TerminalCall>,
) -> TerminalCall {
    tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .expect("timed out waiting for a terminal hub call")
        .expect("fake terminald stopped recording calls")
}

async fn next_agent_call(calls: &mut tokio::sync::mpsc::UnboundedReceiver<AgentCall>) -> AgentCall {
    tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .expect("timed out waiting for an agent hub call")
        .expect("fake agentd stopped recording calls")
}

/// The runtime probes `list_terminals` right after `hello`
/// (`terminald::establish`'s skew insurance), so every terminald test sees
/// those two calls before its own.
async fn expect_terminald_handshake(
    calls: &mut tokio::sync::mpsc::UnboundedReceiver<TerminalCall>,
) {
    assert!(matches!(
        next_terminal_call(calls).await,
        TerminalCall::Hello
    ));
    assert!(matches!(
        next_terminal_call(calls).await,
        TerminalCall::ListTerminals
    ));
}

/// Reads frames off a terminal handle's `frames()` stream until one whose
/// text matches `text` arrives, skipping the empty seed frame the watch
/// always delivers first (wire v11).
async fn recv_frame(
    mut rx: tokio::sync::watch::Receiver<TerminalFrame>,
    text: &str,
) -> TerminalFrame {
    tokio::time::timeout(Duration::from_secs(5), async move {
        // A handle cloned after the route published already starts at the
        // current snapshot; `changed()` would otherwise wait for a newer
        // frame and miss the value this helper was asked to observe.
        let current = rx.borrow_and_update().clone();
        if current.text() == text {
            return current;
        }
        loop {
            rx.changed()
                .await
                .expect("frame watch closed before the expected frame arrived");
            let frame = rx.borrow_and_update().clone();
            if frame.text() == text {
                return frame;
            }
        }
    })
    .await
    .expect("timed out waiting for the expected terminal frame")
}

#[test]
fn local_terminal_frame_route_collapses_a_burst_to_its_latest_snapshot() {
    let routes = TerminalRoutes::new();
    let session_id = Uuid::new_v4();
    let (frame_tx, mut frame_rx) = tokio::sync::watch::channel(TerminalFrame::empty());
    let (event_tx, _event_rx) = unbounded();
    let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
    routes.register_terminal(session_id, frame_tx, event_tx, command_tx);

    for text in ["obsolete-1", "obsolete-2", "latest"] {
        routes.route_terminal_frame(session_id, TerminalFrame::from_text(text.into()));
    }

    assert!(frame_rx.has_changed().unwrap());
    assert_eq!(frame_rx.borrow_and_update().text(), "latest");
    assert!(!frame_rx.has_changed().unwrap());
}

/// The client-side half of the split's central claim: the two daemons are
/// separate route tables, so an agent-runtime failure never reaches a
/// terminal pane. Before the split one `Routes` served both, and
/// `connection_failed` fanned a `TerminalUpdate::Error` out to every
/// terminal — exactly the coupling `Reload Agent Runtime` made visible.
#[test]
fn an_agent_runtime_failure_does_not_touch_terminal_routes() {
    let (host_tools, _host_tools_rx) = unbounded();
    let (workspace_roots, _workspace_roots_rx) = unbounded();
    let agent_routes = AgentRoutes::new(host_tools, workspace_roots);
    let terminal_routes = TerminalRoutes::new();

    let terminal_id = Uuid::new_v4();
    let (frame_tx, _frame_rx) = tokio::sync::watch::channel(TerminalFrame::empty());
    let (terminal_event_tx, terminal_event_rx) = unbounded();
    let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
    terminal_routes.register_terminal(terminal_id, frame_tx, terminal_event_tx, command_tx);
    let agent_id = SessionId::new();
    let (agent_event_tx, agent_event_rx) = unbounded();
    agent_routes.register_agent(agent_id, agent_event_tx);

    agent_routes.connection_failed("the agent runtime died".to_string());

    assert!(matches!(
        agent_event_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .event,
        Event::Error(_)
    ));
    assert!(
        terminal_event_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a agentd failure must not reach terminal panes"
    );

    // And a terminal registered *after* the agent failure is still clean --
    // the sticky failure is per-table too.
    let later_id = Uuid::new_v4();
    let (later_frames, _later_frames_rx) = tokio::sync::watch::channel(TerminalFrame::empty());
    let (later_events, later_events_rx) = unbounded();
    let (later_commands, _later_commands_rx) = tokio::sync::mpsc::unbounded_channel();
    terminal_routes.register_terminal(later_id, later_frames, later_events, later_commands);
    assert!(later_events_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_returns_before_the_connection_and_a_queued_create_arrives_after() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let started = Instant::now();
    let handle = TerminaldHandle::start_on_stream(client);
    assert!(started.elapsed() < Duration::from_millis(50));

    // Queued before the daemon has even completed a handshake.
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());

    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;
    let TerminalCall::CreateTerminal {
        session_id,
        spec: received_spec,
        peer,
    } = next_terminal_call(&mut calls).await
    else {
        panic!("expected the queued create to arrive first after the handshake");
    };
    assert_eq!(session_id, terminal_id);
    assert_eq!(*received_spec, spec());

    let frame = TerminalFrame::from_text("ready".into());
    peer.frames.send(frame.clone()).unwrap();
    assert_eq!(recv_frame(terminal.frames(), "ready").await, frame);
}

/// Routing per call site, the split's client-runtime contract: terminal ops
/// reach the terminal daemon and agent ops the agent daemon, with neither
/// hub ever seeing the other's traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_ops_go_to_terminald_and_agent_ops_go_to_sessiond() {
    let (terminal_client, terminal_server) = tokio::io::duplex(64 * 1024);
    let (agent_client, agent_server) = tokio::io::duplex(64 * 1024);
    let terminald = TerminaldHandle::start_on_stream(terminal_client);
    let (agentd, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(agent_client);

    let terminal_id = Uuid::new_v4();
    let terminal = terminald.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let agent = agentd.start_session(agent_id, ProviderId("mock".into()), None, None, None, false);

    let (mut terminal_calls, _tconn, _tserve) =
        serve_fake_terminal_hub(terminal_server, FakeBehavior::default()).await;
    let (mut agent_calls, _aconn, _aserve) =
        serve_fake_session_hub(agent_server, FakeBehavior::default()).await;

    expect_terminald_handshake(&mut terminal_calls).await;
    let TerminalCall::CreateTerminal {
        session_id, peer, ..
    } = next_terminal_call(&mut terminal_calls).await
    else {
        panic!("the terminal create must land on terminald");
    };
    assert_eq!(session_id, terminal_id);
    let terminal_peer = peer;

    assert!(matches!(
        next_agent_call(&mut agent_calls).await,
        AgentCall::Hello
    ));
    let AgentCall::NewAgent { new, peer } = next_agent_call(&mut agent_calls).await else {
        panic!("the agent spawn must land on agentd");
    };
    assert_eq!(new.session_id, agent_id);
    let agent_peer = peer;

    let frame = TerminalFrame::from_text("terminal".into());
    terminal_peer.frames.send(frame.clone()).unwrap();
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

    assert_eq!(recv_frame(terminal.frames(), "terminal").await, frame);
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

    // Neither daemon saw a call belonging to the other domain.
    assert!(terminal_calls.try_recv().is_err());
    assert!(agent_calls.try_recv().is_err());
}

/// `Reload Agent Runtime`'s client-runtime core (design decision 2): the
/// agent runtime's drain reaches *only* agentd, and the terminal session
/// keeps streaming frames right through it. This is the acceptance property
/// the daemon-level e2e (`horizon-terminald::e2e`'s
/// `a_sessiond_drain_and_respawn_leaves_a_live_terminald_session_attachable`)
/// proves against real processes; here it is proven at the seam that decides
/// *which* daemon a drain is sent to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draining_the_agent_runtime_leaves_the_terminal_runtime_untouched() {
    let (terminal_client, terminal_server) = tokio::io::duplex(64 * 1024);
    let (agent_client, agent_server) = tokio::io::duplex(64 * 1024);
    let terminald = TerminaldHandle::start_on_stream(terminal_client);
    let (agentd, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(agent_client);

    let terminal = terminald.start_terminal(Uuid::new_v4(), spec());
    let (mut terminal_calls, _tconn, _tserve) =
        serve_fake_terminal_hub(terminal_server, FakeBehavior::default()).await;
    let (mut agent_calls, _aconn, _aserve) =
        serve_fake_session_hub(agent_server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut terminal_calls).await;
    let TerminalCall::CreateTerminal { peer, .. } = next_terminal_call(&mut terminal_calls).await
    else {
        panic!("expected the create call");
    };
    assert!(matches!(
        next_agent_call(&mut agent_calls).await,
        AgentCall::Hello
    ));

    assert!(agentd.begin_reload(), "the agent runtime was established");
    assert!(matches!(
        next_agent_call(&mut agent_calls).await,
        AgentCall::Drain
    ));

    // The terminal daemon was never asked to drain, and its session is
    // still live: a frame published after the agent drain still arrives.
    let frame = TerminalFrame::from_text("still alive".into());
    peer.frames.send(frame.clone()).unwrap();
    assert_eq!(recv_frame(terminal.frames(), "still alive").await, frame);
    assert!(
        terminal_calls.try_recv().is_err(),
        "draining agentd must not send anything to terminald"
    );
}

/// `AgentWireEvent::WorkspaceRootResolved` is a live daemon->shell
/// announcement (`docs/session-relationship-design.md`'s "still eventual,
/// not live" gap), not a `contract::ProviderEvent` any per-session route
/// folds -- `AgentRoutes` sends it on its own `workspace_roots` channel
/// instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incoming_workspace_root_resolved_reaches_its_own_channel() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools, workspace_roots) = SessiondHandle::start_on_stream(client);
    let session_id = SessionId::new();
    let _agent = handle.attach_session(session_id);

    let (mut calls, _conn, _serve) = serve_fake_session_hub(server, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));
    let AgentCall::AttachAgent {
        session_id: attached_id,
        peer,
    } = next_agent_call(&mut calls).await
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
    let handle = TerminaldHandle::start_on_stream(client);

    let first_session = Uuid::new_v4();
    let second_session = Uuid::new_v4();
    let behavior = FakeBehavior {
        terminal_lists: vec![
            // The first reply is consumed by the establish-time skew probe.
            Vec::new(),
            vec![TerminalSummary {
                session_id: first_session,
            }],
            vec![TerminalSummary {
                session_id: second_session,
            }],
        ],
        ..FakeBehavior::default()
    };
    let (_calls, _conn, _serve) = serve_fake_terminal_hub(server, behavior).await;

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
    let handle = TerminaldHandle::start_on_stream(client);
    let attached_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();

    let behavior = FakeBehavior {
        missing_terminals: vec![missing_id],
        ..FakeBehavior::default()
    };
    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, behavior).await;

    let attach_handle = handle.clone();
    let attached =
        std::thread::spawn(move || attach_handle.attach_terminals(vec![attached_id, missing_id]));

    expect_terminald_handshake(&mut calls).await;
    let mut peers = HashMap::new();
    for _ in 0..2 {
        let TerminalCall::AttachTerminal { session_id, peer } =
            next_terminal_call(&mut calls).await
        else {
            panic!("expected an attach call");
        };
        peers.insert(session_id, peer);
    }
    assert!(peers[&missing_id].is_none());
    let peer = peers
        .remove(&attached_id)
        .flatten()
        .expect("the attached session should have live channels");

    let frame = TerminalFrame::from_text("survived".into());
    peer.frames.send(frame.clone()).unwrap();

    let mut sessions = attached.join().unwrap();
    assert_eq!(sessions.len(), 1);
    let (session_id, session) = sessions.pop().unwrap();
    assert_eq!(session_id, attached_id);
    assert_eq!(recv_frame(session.frames(), "survived").await, frame);
}

/// Review fix 2: a clean shell exit must retire the pane even when the
/// frames watch closes *before* the `Exited` event lands. The two closures
/// race in the attachment's `select!`; a frames-close winning it must not
/// end the loop before `Exited` is drained, or the pane is stranded as a
/// zombie (shell gone, still displayed). Here the peer closes the frames
/// watch first, then delivers `Exited` on the events channel; the pane's
/// event stream must still receive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_exit_retires_the_pane_even_when_the_frames_watch_closes_first() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let terminal = handle.start_terminal(Uuid::new_v4(), spec());

    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;
    let TerminalCall::CreateTerminal { peer, .. } = next_terminal_call(&mut calls).await else {
        panic!("expected a create call");
    };
    let peer = *peer;

    // Close the frames watch first, let the client observe it, then send
    // Exited: the race the fix guards against. (Before the fix, the
    // frames-close broke the loop and the Exited was never routed.)
    drop(peer.frames);
    tokio::time::sleep(Duration::from_millis(100)).await;
    peer.events.send(TerminalUpdate::Exited).await.unwrap();

    let update = terminal
        .events()
        .recv_timeout(Duration::from_secs(5))
        .expect("Exited must reach the pane even though the frames watch closed first");
    assert!(matches!(update, TerminalUpdate::Exited), "got {update:?}");
}

/// Review fix 3: the frames watch inlines its seed (the retained latest
/// frame) into the `attach_terminal` rtc reply, so the reply cap must admit
/// a frame the *live* watch already accepts. A retained frame between the
/// old 1 MiB reply cap and `FRAME_MAX_ITEM_BYTES` (4 MiB) must re-attach
/// successfully and deliver the frame — before the fix this failed
/// permanently while live delivery of the same frame succeeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_reseeds_a_large_retained_frame_within_the_reply_cap() {
    // 2 MiB: comfortably above the old 1 MiB reply cap, below the 4 MiB
    // frame cap.
    let big = TerminalFrame::from_text("Z".repeat(2 * 1024 * 1024));
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let behavior = FakeBehavior {
        attach_seed: Some(big.clone()),
        ..FakeBehavior::default()
    };
    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, behavior).await;

    let id = Uuid::new_v4();
    let attach_handle = handle.clone();
    let attached = std::thread::spawn(move || attach_handle.attach_terminals(vec![id]));

    expect_terminald_handshake(&mut calls).await;
    let TerminalCall::AttachTerminal { session_id, peer } = next_terminal_call(&mut calls).await
    else {
        panic!("expected an attach call");
    };
    assert_eq!(session_id, id);
    assert!(
        peer.is_some(),
        "attach must succeed for a frame the live watch would accept"
    );

    let sessions = attached.join().unwrap();
    assert_eq!(sessions.len(), 1, "the large-frame attach must be claimed");
    let (_, session) = &sessions[0];
    assert_eq!(recv_frame(session.frames(), &big.text()).await, big);
}

/// Review fix 5: a `Clipboard` event larger than the old 1 MiB events cap
/// (an OSC 52 copy of a big selection) must reach the pane — v10 carried it
/// on the 4 MiB `updates` mpsc, and the events cap must not silently shrink
/// that to a quarter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_clipboard_event_reaches_the_pane() {
    let big_text = "C".repeat(2 * 1024 * 1024);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let terminal = handle.start_terminal(Uuid::new_v4(), spec());

    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;
    let TerminalCall::CreateTerminal { peer, .. } = next_terminal_call(&mut calls).await else {
        panic!("expected a create call");
    };
    let peer = *peer;

    peer.events
        .send(TerminalUpdate::Clipboard {
            text: big_text.clone(),
            destination: ClipboardDestination::Clipboard,
        })
        .await
        .expect("the fake daemon should accept a large clipboard send");

    let update = terminal
        .events()
        .recv_timeout(Duration::from_secs(5))
        .expect("a >1 MiB clipboard event must reach the pane");
    match update {
        TerminalUpdate::Clipboard { text, destination } => {
            assert_eq!(text.len(), big_text.len());
            assert_eq!(destination, ClipboardDestination::Clipboard);
        }
        other => panic!("expected a Clipboard event, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_runtime_does_not_send_drain() {
    let (client, server) = tokio::io::duplex(4096);
    let (handle, _host_tools, _workspace_roots) = SessiondHandle::start_on_stream(client);
    let responder = handle.responder();
    let (mut calls, _conn, serve) = serve_fake_session_hub(server, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));

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
        !saw.iter().any(|call| matches!(call, AgentCall::Drain)),
        "dropping the runtime must not drain the daemon: {saw:?}"
    );
    drop(responder);
}

/// The terminal runtime gets the same guarantee: dropping it (a window
/// closing, a handle going out of scope) must never kill the PTYs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_terminal_runtime_does_not_send_drain() {
    let (client, server) = tokio::io::duplex(4096);
    let handle = TerminaldHandle::start_on_stream(client);
    let (mut calls, _conn, serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;

    drop(handle);

    tokio::time::timeout(Duration::from_secs(5), serve)
        .await
        .expect("the fake daemon's serve loop should end after the runtime drops")
        .unwrap();
    let mut saw = Vec::new();
    while let Ok(call) = calls.try_recv() {
        saw.push(call);
    }
    assert!(
        !saw.iter().any(|call| matches!(call, TerminalCall::Drain)),
        "dropping the terminal runtime must not drain the daemon: {saw:?}"
    );
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
    let agent_id = SessionId::new();
    let agent = handle.start_session(agent_id, ProviderId("mock".into()), None, None, None, false);

    let (mut calls, conn, serve) = serve_fake_session_hub(server, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));
    next_agent_call(&mut calls).await;

    // The daemon dies: mux and serve loop torn down.
    conn.abort();
    serve.abort();

    let agent_error = agent.events().recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(agent_error.event, Event::Error(_)));

    assert!(handle
        .session_list()
        .unwrap_err()
        .contains("runtime stopped"));
}

/// The terminal runtime's counterpart: an established terminald that dies
/// fans the failure into every terminal pane, and later panes inherit it
/// rather than hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_established_terminald_disconnect_reports_errors_without_reconnecting() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let terminal = handle.start_terminal(Uuid::new_v4(), spec());

    let (mut calls, conn, serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;
    next_terminal_call(&mut calls).await;

    conn.abort();
    serve.abort();

    let terminal_error = terminal
        .events()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(matches!(terminal_error, TerminalUpdate::Error(_)));

    let late_terminal = handle.start_terminal(Uuid::new_v4(), spec());
    assert!(matches!(
        late_terminal
            .events()
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
    let (_calls, _conn, _serve) = serve_fake_session_hub(server, behavior).await;

    let error = handle.session_list().unwrap_err();
    assert!(
        error.contains("runtime stopped"),
        "the runtime should stop after a rejected hello; error was: {error}"
    );
    let agent = handle.start_session(
        SessionId::new(),
        ProviderId("mock".into()),
        None,
        None,
        None,
        false,
    );
    let event = agent.events().recv_timeout(Duration::from_secs(5)).unwrap();
    let Event::Error(error) = event.event else {
        panic!("expected the rejection to fan out as an error, got {event:?}");
    };
    assert!(
        error.message.contains("rejected the handshake"),
        "error was: {}",
        error.message
    );
}

/// Decision 6's skew insurance, end to end through the runtime: a terminald
/// that negotiates `hello` but cannot answer the first call after it is
/// refused *cleanly* — the runtime stops, and the message names both the
/// daemon's reported `binary_id` and `Reload Terminal Runtime` as the fix,
/// rather than leaving panes attached to a peer this build cannot talk to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminald_that_fails_the_call_after_hello_is_refused_by_name() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let terminal = handle.start_terminal(Uuid::new_v4(), spec());
    let behavior = FakeBehavior {
        fail_after_hello: true,
        ..FakeBehavior::default()
    };
    let (_calls, _conn, _serve) = serve_fake_terminal_hub(server, behavior).await;

    let update = terminal
        .events()
        .recv_timeout(Duration::from_secs(10))
        .expect("the refusal must reach the pane, not hang");
    let TerminalUpdate::Error(message) = update else {
        panic!("expected the refusal to fan out as an error, got {update:?}");
    };
    assert!(
        message.contains("fake-terminald"),
        "the refusal must name the peer's binary id; error was: {message}"
    );
    assert!(
        message.contains("Reload Terminal Runtime"),
        "the refusal must name the remedy; error was: {message}"
    );
}

fn stub_socket_paths(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    // Keep well under SUN_LEN, same as the agentd e2e tests.
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

/// Holds accepted stub connections open, silently reading — the measured
/// presentation of a real v9 JSONL daemon (its pre-hello `read_line`
/// blocks forever on chmux bytes, which contain no newline).
fn hold_silently(stream: tokio::net::UnixStream) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stream = stream;
        let mut chunk = [0_u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
}

/// The cross-generation recovery loop (`docs/remoc-adoption-design.md` §6,
/// re-anchoring PR #18's scenarios on the new detection path): a v10+
/// runtime meets a still-running JSONL daemon — which blocks silently in
/// its pre-hello `read_line`, the measured real-v9 presentation — sees
/// `SILENCE_MISMATCH_THRESHOLD` consecutive bounded-timeout silences
/// (single timeouts are transient and never consume the recovery budget),
/// probes a legacy `Drain` at the newest JSONL version, and adopts the
/// respawned (remoc) daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jsonl_generation_daemon_is_probed_drained_and_the_respawn_adopted() {
    // Shrink the establish deadline so three consecutive silences take
    // fractions of a second, not 15 s of wall clock.
    std::env::set_var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS", "300");
    let (socket_path, control_socket) = stub_socket_paths("probe");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);

    // Connections 1..3: the silent JSONL daemon, once per establish
    // attempt (the runtime redials between timeouts).
    let mut held = Vec::new();
    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }

    // Connection 4: the first drain probe — one newline-terminated JSONL
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
    let (mut calls, _conn, _serve) = serve_fake_session_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));

    // Prove the recovered connection is fully established with a round
    // trip.
    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Recovery is attempted exactly once per runtime: if the replacement
/// daemon still can't speak remoc (a stale horizon-agentd binary that a
/// rebuild never touched), the runtime must fail loudly instead of
/// drain-and-restarting forever, with the rebuild hint in the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_generation_mismatch_after_recovery_goes_fatal_instead_of_looping() {
    std::env::set_var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS", "300");
    let (socket_path, control_socket) = stub_socket_paths("fatal");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);
    let agent = handle.start_session(
        SessionId::new(),
        ProviderId("mock".into()),
        None,
        None,
        None,
        false,
    );

    // Connections 1..3: silent JSONL-generation daemon, up to the
    // consecutive-silence threshold.
    let mut held = Vec::new();
    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }
    // Connection 4: the one drain probe this runtime is allowed.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_until_closed(&mut stream).await;
    }
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);

    // Connections 5..7: the "respawned" daemon is just as stale (silent).
    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }

    // The runtime gives up rather than draining again, with the rebuild
    // hint, fanned out to the registered routes.
    let event = agent
        .events()
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    let Event::Error(error) = event.event else {
        panic!("expected the fatal mismatch to fan out as an error, got {event:?}");
    };
    assert!(
        error.message.contains("already attempted") && error.message.contains("rebuild"),
        "error was: {}",
        error.message
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
    let (_calls_1, _conn_1, _serve_1) = serve_fake_session_hub(stream, behavior).await;

    // Connection 2: the recovery drain arrives as an rtc call.
    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (mut drain_calls, _conn_2, _serve_2) = serve_fake_session_hub(stream, behavior).await;
    let drain = next_agent_call(&mut drain_calls).await;
    assert!(matches!(drain, AgentCall::Drain), "got {drain:?}");

    // "Exit", then come back as a compatible daemon.
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);
    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn_3, _serve_3) =
        serve_fake_session_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// The terminal runtime's own recovery: a terminald whose hub rejects the
/// range is drained over the version-stable rtc surface and the respawn is
/// adopted. Unlike agentd there is no JSONL generation to probe downward
/// through — this one path covers every stale terminald.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_range_rejecting_terminald_is_drained_via_rtc_and_the_respawn_adopted() {
    let (socket_path, control_socket) = stub_socket_paths("trej");
    let listener = bind_stub_listener(&socket_path);
    let handle = TerminaldHandle::start(&socket_path, &control_socket);

    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (_calls_1, _conn_1, _serve_1) = serve_fake_terminal_hub(stream, behavior).await;

    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        reject_hello: true,
        ..FakeBehavior::default()
    };
    let (mut drain_calls, _conn_2, _serve_2) = serve_fake_terminal_hub(stream, behavior).await;
    let drain = next_terminal_call(&mut drain_calls).await;
    assert!(matches!(drain, TerminalCall::Drain), "got {drain:?}");

    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);
    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn_3, _serve_3) =
        serve_fake_terminal_hub(stream, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.terminal_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Host-side coverage for `TerminaldHandle::broadcast_terminal_color_scheme`
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
    let handle = TerminaldHandle::start_on_stream(client);

    let attached_a = Uuid::new_v4();
    let attached_b = Uuid::new_v4();
    let missing = Uuid::new_v4();
    let behavior = FakeBehavior {
        missing_terminals: vec![missing],
        ..FakeBehavior::default()
    };
    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, behavior).await;

    let attach_handle = handle.clone();
    let attached = std::thread::spawn(move || {
        attach_handle.attach_terminals(vec![attached_a, attached_b, missing])
    });

    expect_terminald_handshake(&mut calls).await;
    let mut peers = HashMap::new();
    for _ in 0..3 {
        let TerminalCall::AttachTerminal { session_id, peer } =
            next_terminal_call(&mut calls).await
        else {
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

/// Review fix (establishment classification): transient failures —
/// connections the daemon drops before/during the handshake — are retried
/// with backoff and never consume the once-per-runtime recovery budget.
/// Two immediate closes followed by a healthy daemon must end established,
/// where the old classification would have burned the budget on close #1
/// and gone fatal on close #2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_transient_failures_do_not_consume_the_recovery_budget() {
    let (socket_path, control_socket) = stub_socket_paths("transient");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);

    // Connections 1 and 2: accepted and immediately dropped (a crashing
    // daemon; `ChMux(StreamClosed)` on the runtime's side).
    for _ in 0..2 {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    }

    // Connection 3: a healthy daemon — must be adopted normally.
    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn, _serve) = serve_fake_session_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Review fix (establishment classification): a connection dropping while
/// the `hello` call itself is in flight (`HubError::Call`) is a transient,
/// retried like any other pre-hello drop — the old classification sent it
/// straight to a fatal stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_drop_during_hello_is_retried_not_fatal() {
    let (socket_path, control_socket) = stub_socket_paths("hellodrop");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        SessiondHandle::start(&socket_path, &control_socket);

    // Connection 1: a daemon that completes the handshake and hands over
    // its hub, then dies while hello is pending.
    let (stream, _) = listener.accept().await.unwrap();
    let behavior = FakeBehavior {
        hang_hello: true,
        ..FakeBehavior::default()
    };
    let (_calls, conn, serve) = serve_fake_session_hub(stream, behavior).await;
    // Give the runtime a moment to get its hello call in flight, then
    // kill the daemon under it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    conn.abort();
    serve.abort();

    // Connection 2: a healthy daemon — the runtime must have retried
    // rather than stopped.
    let (stream, _) = listener.accept().await.unwrap();
    let (mut calls, _conn, _serve) = serve_fake_session_hub(stream, FakeBehavior::default()).await;
    assert!(matches!(
        next_agent_call(&mut calls).await,
        AgentCall::Hello
    ));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Review fix (size caps), pinning the *measured* oversized-request
/// semantics: the daemon drops a request over `RTC_MAX_REQUEST_BYTES`
/// per-item, so the op fails loudly (the pane gets an error, never a
/// hang) — and because rch latches the remote-send error onto the
/// transported request channel, the connection then tears down (the
/// runtime stops with the failure fanned out). Deliberate bluntness:
/// every rtc request is a small fixed-shape struct, so exceeding the cap
/// is a bug, never data — see `RTC_MAX_REQUEST_BYTES`'s doc.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversized_rtc_request_fails_the_op_and_stops_the_runtime() {
    let (client, server) = tokio::io::duplex(1024 * 1024);
    let handle = TerminaldHandle::start_on_stream(client);
    let (mut calls, _conn, _serve) = serve_fake_terminal_hub(server, FakeBehavior::default()).await;
    expect_terminald_handshake(&mut calls).await;

    // A spawn spec far over the 64 KiB request cap.
    let mut oversized = spec();
    oversized.args = vec!["x".repeat(200 * 1024)];
    let terminal = handle.start_terminal(Uuid::new_v4(), oversized);
    let update = terminal
        .events()
        .recv_timeout(Duration::from_secs(10))
        .expect("the oversized create must fail loudly, not hang");
    assert!(
        matches!(update, TerminalUpdate::Error(_)),
        "expected a create failure, got {update:?}"
    );

    // The latched request channel ends the connection; later panes get
    // the failure rather than a hang.
    let late = handle.start_terminal(Uuid::new_v4(), spec());
    assert!(matches!(
        late.events().recv_timeout(Duration::from_secs(10)).unwrap(),
        TerminalUpdate::Error(_)
    ));
}
