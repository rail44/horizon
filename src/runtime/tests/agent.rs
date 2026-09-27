//! Agent-runtime tests against a fake ACP v2 agent (`Agent.v2()`), served
//! over one end of a `UnixStream` pair or a stub listener -- the transport
//! production uses. The fake records every request and notification it
//! receives; tests drive updates, permission requests, and host-tool
//! requests through its connection handle.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::{v2, ProtocolVersion};
use agent_client_protocol::{Agent, ByteStreams, Client, Responder, V2ConnectionTo};
use horizon_acp::{
    read_horizon_meta, write_horizon_meta, AttachmentEnd, ContinueTurnRequest, DrainRequest,
    EmptyResponse, EnsureBoardOrganizerRequest, EnsureBoardOrganizerResponse, HostToolRequest,
    HostToolResponse, InitializeMeta, PermissionResponseMeta, SessionEventNotification, SessionId,
    SessionInfoMeta, SessionNewMeta, HORIZON_ACP_EXT_VERSION, PERMISSION_OPTION_APPROVE,
    PERMISSION_OPTION_DENY,
};
use horizon_agent::contract::ProviderId;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::super::attachment::acp_session_id;
use super::super::*;
use super::{
    bind_stub_listener, expect_terminald_handshake, hold_silently, next_terminal_call,
    read_until_closed, recv_frame, serve_fake_terminal_hub, spec, stub_socket_paths, FakeBehavior,
    TerminalCall,
};
use crate::agent::model::{AgentEvent, PermissionDecision, ToolCallIdentity};

#[derive(Clone, Copy, Default)]
struct FakeAgentBehavior {
    /// Answer `initialize` with an error-shaped extension-version rejection.
    reject_initialize: bool,
    /// Report a different extension version on a successful `initialize`
    /// and reject every later request except `_horizon/drain`.
    mismatched_ext_version: bool,
    /// Initialize normally but reject `session/list` with the
    /// extension-version mismatch error.
    reject_requests: bool,
    /// Never answer `initialize`.
    hang_initialize: bool,
    /// Hand `session/new` and `session/resume` responders to the test
    /// instead of answering them.
    manual_bootstrap: bool,
}

enum AgentCall {
    Initialize,
    NewSession {
        meta: SessionNewMeta,
        cwd: PathBuf,
        responder: Option<Responder<v2::NewSessionResponse>>,
    },
    Resume {
        session_id: SessionId,
        responder: Option<Responder<v2::ResumeSessionResponse>>,
    },
    List,
    Prompt(String),
    Cancel,
    Close,
    ContinueTurn,
    Drain,
    EnsureBoardOrganizer(PathBuf, SessionId),
}

impl std::fmt::Debug for AgentCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            AgentCall::Initialize => "Initialize",
            AgentCall::NewSession { .. } => "NewSession",
            AgentCall::Resume { .. } => "Resume",
            AgentCall::List => "List",
            AgentCall::Prompt(_) => "Prompt",
            AgentCall::Cancel => "Cancel",
            AgentCall::Close => "Close",
            AgentCall::ContinueTurn => "ContinueTurn",
            AgentCall::Drain => "Drain",
            AgentCall::EnsureBoardOrganizer(..) => "EnsureBoardOrganizer",
        };
        f.write_str(name)
    }
}

struct FakeAgentd {
    calls: UnboundedReceiver<AgentCall>,
    connection: V2ConnectionTo<Client>,
    task: JoinHandle<()>,
}

fn mismatch_error() -> agent_client_protocol::Error {
    agent_client_protocol::Error::new(
        -32600,
        format!(
            "horizon ext version mismatch: daemon {} client {}",
            HORIZON_ACP_EXT_VERSION + 5,
            HORIZON_ACP_EXT_VERSION
        ),
    )
}

fn session_uuid(id: &v2::SessionId) -> SessionId {
    SessionId::from_uuid(uuid::Uuid::parse_str(&id.0).unwrap())
}

async fn serve_fake_agentd(
    stream: tokio::net::UnixStream,
    behavior: FakeAgentBehavior,
) -> FakeAgentd {
    type Calls = tokio::sync::mpsc::UnboundedSender<AgentCall>;
    let (calls_tx, calls) = tokio::sync::mpsc::unbounded_channel::<AgentCall>();
    let held_initialize: Arc<Mutex<Vec<Responder<v2::InitializeResponse>>>> = Arc::default();
    let record = |calls: &Calls, call| {
        let _ = calls.send(call);
    };
    let (c1, c2, c3, c4, c5, c6, c7, c8, c9, c10) = (
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
        calls_tx.clone(),
    );
    let builder = Agent
        .v2()
        .name("fake-agentd")
        .on_receive_request(
            async move |_request: v2::InitializeRequest,
                        responder: Responder<v2::InitializeResponse>,
                        _connection: V2ConnectionTo<Client>| {
                if behavior.hang_initialize {
                    held_initialize.lock().unwrap().push(responder);
                    return Ok(());
                }
                if behavior.reject_initialize {
                    return responder.respond_with_error(mismatch_error());
                }
                record(&c1, AgentCall::Initialize);
                let ext_version = if behavior.mismatched_ext_version {
                    HORIZON_ACP_EXT_VERSION + 5
                } else {
                    HORIZON_ACP_EXT_VERSION
                };
                let mut response = v2::InitializeResponse::new(
                    ProtocolVersion::V2,
                    v2::Implementation::new("fake-agentd", "0.0.0"),
                )
                .capabilities(v2::AgentCapabilities::new().session(v2::SessionCapabilities::new()));
                write_horizon_meta(
                    &mut response.meta,
                    &InitializeMeta {
                        ext_version,
                        binary_id: "fake-agentd".into(),
                    },
                )
                .unwrap();
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: v2::NewSessionRequest,
                        responder: Responder<v2::NewSessionResponse>,
                        _connection: V2ConnectionTo<Client>| {
                let meta: SessionNewMeta = read_horizon_meta(request.meta.as_ref())
                    .expect("session/new carries SessionNewMeta")
                    .unwrap();
                let cwd = request.cwd.0.clone();
                if behavior.manual_bootstrap {
                    record(
                        &c2,
                        AgentCall::NewSession {
                            meta,
                            cwd,
                            responder: Some(responder),
                        },
                    );
                    return Ok(());
                }
                let mut response =
                    v2::NewSessionResponse::new(meta.session_id.as_uuid().to_string());
                write_horizon_meta(
                    &mut response.meta,
                    &SessionInfoMeta {
                        workspace_root: Some(cwd.clone()),
                        parent_session_id: None,
                        role_id: meta.role_id.clone(),
                        provider_id: meta.provider_id.clone(),
                    },
                )
                .unwrap();
                record(
                    &c2,
                    AgentCall::NewSession {
                        meta,
                        cwd,
                        responder: None,
                    },
                );
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: v2::ResumeSessionRequest,
                        responder: Responder<v2::ResumeSessionResponse>,
                        _connection: V2ConnectionTo<Client>| {
                assert!(
                    request.replay_from.is_some(),
                    "resume replays from the start"
                );
                let session_id = session_uuid(&request.session_id);
                if behavior.manual_bootstrap {
                    record(
                        &c3,
                        AgentCall::Resume {
                            session_id,
                            responder: Some(responder),
                        },
                    );
                    return Ok(());
                }
                record(
                    &c3,
                    AgentCall::Resume {
                        session_id,
                        responder: None,
                    },
                );
                responder.respond(v2::ResumeSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_request: v2::ListSessionsRequest,
                        responder: Responder<v2::ListSessionsResponse>,
                        _connection: V2ConnectionTo<Client>| {
                record(&c4, AgentCall::List);
                if behavior.mismatched_ext_version || behavior.reject_requests {
                    return responder.respond_with_error(mismatch_error());
                }
                responder.respond(v2::ListSessionsResponse::new(Vec::new()))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: v2::PromptRequest,
                        responder: Responder<v2::PromptResponse>,
                        _connection: V2ConnectionTo<Client>| {
                let text = request
                    .prompt
                    .iter()
                    .filter_map(|block| match block {
                        v2::ContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect();
                record(&c5, AgentCall::Prompt(text));
                responder.respond(v2::PromptResponse::new(uuid::Uuid::new_v4().to_string()))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_notification: v2::CancelSessionNotification,
                        _connection: V2ConnectionTo<Client>| {
                record(&c6, AgentCall::Cancel);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |_request: v2::CloseSessionRequest,
                        responder: Responder<v2::CloseSessionResponse>,
                        _connection: V2ConnectionTo<Client>| {
                record(&c7, AgentCall::Close);
                responder.respond(v2::CloseSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_request: ContinueTurnRequest,
                        responder: Responder<EmptyResponse>,
                        _connection: V2ConnectionTo<Client>| {
                record(&c8, AgentCall::ContinueTurn);
                responder.respond(EmptyResponse {})
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_request: DrainRequest,
                        responder: Responder<EmptyResponse>,
                        _connection: V2ConnectionTo<Client>| {
                record(&c9, AgentCall::Drain);
                responder.respond(EmptyResponse {})
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: EnsureBoardOrganizerRequest,
                        responder: Responder<EnsureBoardOrganizerResponse>,
                        _connection: V2ConnectionTo<Client>| {
                let session_id = SessionId::new();
                record(
                    &c10,
                    AgentCall::EnsureBoardOrganizer(request.workspace_root, session_id),
                );
                responder.respond(EnsureBoardOrganizerResponse { session_id })
            },
            agent_client_protocol::on_receive_request!(),
        );
    drop(calls_tx);
    let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();
    let (read_half, write_half) = stream.into_split();
    let transport = ByteStreams::new(write_half.compat_write(), read_half.compat());
    let task = tokio::spawn(async move {
        let _ = builder
            .connect_with(
                transport,
                async move |connection: V2ConnectionTo<Client>| {
                    let _ = connection_tx.send(connection.clone());
                    connection.incoming_closed().await;
                    Ok(())
                },
            )
            .await;
    });
    let connection = connection_rx.await.expect("the fake agentd connected");
    FakeAgentd {
        calls,
        connection,
        task,
    }
}

impl FakeAgentd {
    async fn next_call(&mut self) -> AgentCall {
        tokio::time::timeout(Duration::from_secs(5), self.calls.recv())
            .await
            .expect("timed out waiting for an agentd call")
            .expect("fake agentd stopped recording calls")
    }

    fn update(&self, session_id: SessionId, update: v2::SessionUpdate) {
        self.connection
            .send_notification(v2::UpdateSessionNotification::new(
                acp_session_id(session_id),
                update,
            ))
            .unwrap();
    }

    fn permission(
        &self,
        session_id: SessionId,
        identity: &ToolCallIdentity,
    ) -> agent_client_protocol::SentRequest<v2::RequestPermissionResponse> {
        let mut request = v2::RequestPermissionRequest::new(
            acp_session_id(session_id),
            "Run bash",
            vec![
                v2::PermissionOption::new(
                    PERMISSION_OPTION_APPROVE,
                    "Approve",
                    v2::PermissionOptionKind::AllowOnce,
                ),
                v2::PermissionOption::new(
                    PERMISSION_OPTION_DENY,
                    "Deny",
                    v2::PermissionOptionKind::RejectOnce,
                ),
            ],
        );
        write_horizon_meta(
            &mut request.meta,
            &horizon_acp::ApprovalMeta {
                call_id: identity.call_id.clone(),
                occurrence_id: identity.occurrence_id.clone(),
                kind: horizon_acp::ApprovalKind::Standard,
            },
        )
        .unwrap();
        self.connection.send_request(request)
    }
}

fn idle() -> v2::SessionUpdate {
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(v2::IdleStateUpdate::new()))
}

async fn next_agent_update(handle: &mut AgentSessionHandle) -> AgentUpdate {
    let events = handle.events.as_mut().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let update = events.recv().await.expect("agent update stream ended");
            if matches!(
                update,
                AgentUpdate::State(
                    AttachmentState::Connecting
                        | AttachmentState::Restoring
                        | AttachmentState::Ready
                )
            ) {
                continue;
            }
            return update;
        }
    })
    .await
    .expect("agent update timed out")
}

async fn next_agent_event(handle: &mut AgentSessionHandle) -> AgentEvent {
    match next_agent_update(handle).await {
        AgentUpdate::Event(event) => *event,
        update => panic!("expected an agent event, got {update:?}"),
    }
}

async fn wait_until_ready(handle: &mut AgentSessionHandle) {
    let events = handle.events.as_mut().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.expect("agent update stream ended") {
                AgentUpdate::State(AttachmentState::Ready) => return,
                AgentUpdate::State(state) if state.is_closed() => {
                    panic!("the attachment closed before it was ready: {state:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the attachment never became ready")
}

fn pair() -> (tokio::net::UnixStream, tokio::net::UnixStream) {
    tokio::net::UnixStream::pair().unwrap()
}

fn start_mock_session(agentd: &AgentdHandle, session_id: SessionId) -> AgentSessionHandle {
    agentd.start_session(
        session_id,
        ProviderId("mock".into()),
        None,
        Some(PathBuf::from("/work/project")),
        None,
        false,
    )
}

fn is_idle_update(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::Update(update)
            if matches!(**update, v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(_)))
    )
}

/// Routing per call site: terminal ops reach the terminal daemon and agent
/// ops the agent daemon, with neither seeing the other's traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_ops_go_to_terminald_and_agent_ops_go_to_agentd() {
    let (terminal_client, terminal_server) = tokio::io::duplex(64 * 1024);
    let (agent_client, agent_server) = pair();
    let terminald = TerminaldHandle::start_on_stream(terminal_client);
    let (agentd, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(agent_client);

    let terminal_id = uuid::Uuid::new_v4();
    let terminal = terminald.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let mut agent = start_mock_session(&agentd, agent_id);

    let (mut terminal_calls, _tconn, _tserve) =
        serve_fake_terminal_hub(terminal_server, FakeBehavior::default()).await;
    let mut fake = serve_fake_agentd(agent_server, FakeAgentBehavior::default()).await;

    expect_terminald_handshake(&mut terminal_calls).await;
    let TerminalCall::CreateTerminal {
        session_id, peer, ..
    } = next_terminal_call(&mut terminal_calls).await
    else {
        panic!("the terminal create must land on terminald");
    };
    assert_eq!(session_id, terminal_id);

    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    let AgentCall::NewSession { meta, cwd, .. } = fake.next_call().await else {
        panic!("the agent spawn must land on agentd");
    };
    assert_eq!(meta.session_id, agent_id);
    assert_eq!(meta.provider_id, "mock");
    assert_eq!(cwd, PathBuf::from("/work/project"));

    let frame = horizon_terminal_core::TerminalFrame::from_text("terminal".into());
    peer.frames.send(frame.clone()).unwrap();
    wait_until_ready(&mut agent).await;
    fake.update(agent_id, idle());
    terminal
        .sender()
        .send(TerminalCommand::Input(b"fifo".to_vec()))
        .unwrap();

    assert_eq!(recv_frame(terminal.frames(), "terminal").await, frame);
    let event = next_agent_event(&mut agent).await;
    assert!(is_idle_update(&event), "got {event:?}");
    let mut commands = peer.commands;
    let command = tokio::time::timeout(Duration::from_secs(5), commands.recv())
        .await
        .expect("timed out waiting for the terminal command")
        .unwrap()
        .expect("terminal command");
    assert_eq!(command, TerminalCommand::Input(b"fifo".to_vec()));

    assert!(terminal_calls.try_recv().is_err());
    assert!(fake.calls.try_recv().is_err());
}

/// `Reload Agent Runtime`'s client-runtime core: the drain reaches only
/// agentd and the terminal session keeps streaming frames through it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draining_the_agent_runtime_leaves_the_terminal_runtime_untouched() {
    let (terminal_client, terminal_server) = tokio::io::duplex(64 * 1024);
    let (agent_client, agent_server) = pair();
    let terminald = TerminaldHandle::start_on_stream(terminal_client);
    let (agentd, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(agent_client);

    let terminal = terminald.start_terminal(uuid::Uuid::new_v4(), spec());
    let (mut terminal_calls, _tconn, _tserve) =
        serve_fake_terminal_hub(terminal_server, FakeBehavior::default()).await;
    let mut fake = serve_fake_agentd(agent_server, FakeAgentBehavior::default()).await;
    expect_terminald_handshake(&mut terminal_calls).await;
    let TerminalCall::CreateTerminal { peer, .. } = next_terminal_call(&mut terminal_calls).await
    else {
        panic!("expected the create call");
    };
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    // A full round trip proves the runtime is established, which is what
    // `begin_reload` reads.
    let list_handle = agentd.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));
    assert!(matches!(fake.next_call().await, AgentCall::List));

    assert!(agentd.begin_reload(), "the agent runtime was established");
    assert!(matches!(fake.next_call().await, AgentCall::Drain));

    let frame = horizon_terminal_core::TerminalFrame::from_text("still alive".into());
    peer.frames.send(frame.clone()).unwrap();
    assert_eq!(recv_frame(terminal.frames(), "still alive").await, frame);
    assert!(
        terminal_calls.try_recv().is_err(),
        "draining agentd must not send anything to terminald"
    );
}

/// A session's resolved workspace root reaches the process-wide channel
/// from both places the contract carries it: `session/new`'s response and a
/// later `session_info_update`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_root_updates_reach_their_own_channel() {
    let (client, server) = pair();
    let (handle, _host_tools, workspace_roots) = AgentdHandle::start_on_stream(client);
    let session_id = SessionId::new();
    let mut agent = start_mock_session(&handle, session_id);

    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    assert!(matches!(
        fake.next_call().await,
        AgentCall::NewSession { .. }
    ));
    wait_until_ready(&mut agent).await;
    let (received, update) = workspace_roots
        .recv_timeout(Duration::from_secs(5))
        .expect("the session/new response's root");
    assert_eq!(received, session_id);
    assert_eq!(update.workspace_root, PathBuf::from("/work/project"));

    let parent_id = SessionId::new();
    let mut meta = None;
    write_horizon_meta(
        &mut meta,
        &SessionInfoMeta {
            workspace_root: Some("/tmp/repo/.horizon/worktrees/abcd1234".into()),
            parent_session_id: Some(parent_id),
            role_id: None,
            provider_id: "mock".into(),
        },
    )
    .unwrap();
    let mut info = v2::SessionInfoUpdate::new();
    info.meta = agent_client_protocol::schema::MaybeUndefined::Value(meta.unwrap());
    fake.update(session_id, v2::SessionUpdate::SessionInfoUpdate(info));

    let (received, update) = workspace_roots
        .recv_timeout(Duration::from_secs(5))
        .expect("the session_info_update's root");
    assert_eq!(received, session_id);
    assert_eq!(
        update,
        WorkspaceRootUpdate {
            workspace_root: "/tmp/repo/.horizon/worktrees/abcd1234".into(),
            parent_session_id: Some(parent_id),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_runtime_does_not_send_drain() {
    let (client, server) = pair();
    let (handle, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(client);
    let responder = handle.responder();
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    drop(handle);

    tokio::time::timeout(Duration::from_secs(5), fake.task)
        .await
        .expect("the fake daemon should stop after the runtime drops")
        .unwrap();
    let mut saw = Vec::new();
    while let Ok(call) = fake.calls.try_recv() {
        saw.push(call);
    }
    assert!(
        !saw.iter().any(|call| matches!(call, AgentCall::Drain)),
        "dropping the runtime must not drain the daemon: {saw:?}"
    );
    drop(responder);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_before_the_daemon_answers_cancels_the_runtime() {
    let (client, _server) = pair();
    let (handle, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(client);
    let stopped = std::thread::spawn(move || handle.stop_and_wait());
    tokio::task::spawn_blocking(move || stopped.join().unwrap())
        .await
        .unwrap();
}

/// A live connection loss disconnects a ready attachment and fails one
/// still restoring; the runtime stops and later requests report it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn established_disconnect_reports_errors_without_reconnecting() {
    let (client, server) = pair();
    let (handle, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(client);
    let mut ready = start_mock_session(&handle, SessionId::new());
    let mut restoring = handle.attach_session(SessionId::new());

    let mut fake = serve_fake_agentd(
        server,
        FakeAgentBehavior {
            manual_bootstrap: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    let mut new_responder = None;
    let mut resume_responder = None;
    for _ in 0..2 {
        match fake.next_call().await {
            AgentCall::NewSession {
                meta, responder, ..
            } => new_responder = Some((meta, responder.unwrap())),
            AgentCall::Resume { responder, .. } => resume_responder = responder,
            call => panic!("unexpected {call:?}"),
        }
    }
    let (meta, responder) = new_responder.unwrap();
    responder
        .respond(v2::NewSessionResponse::new(
            meta.session_id.as_uuid().to_string(),
        ))
        .unwrap();
    wait_until_ready(&mut ready).await;

    fake.task.abort();
    drop(resume_responder);

    assert!(matches!(
        next_agent_update(&mut ready).await,
        AgentUpdate::State(AttachmentState::Disconnected(_))
    ));
    assert!(matches!(
        next_agent_update(&mut restoring).await,
        AgentUpdate::State(AttachmentState::Failed(_))
    ));
    assert!(handle
        .session_list()
        .unwrap_err()
        .contains("runtime stopped"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_initialize_on_a_test_stream_is_a_terminal_failure() {
    let (client, server) = pair();
    let (handle, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(client);
    let _fake = serve_fake_agentd(
        server,
        FakeAgentBehavior {
            reject_initialize: true,
            ..Default::default()
        },
    )
    .await;

    let error = handle.session_list().unwrap_err();
    assert!(
        error.contains("runtime stopped"),
        "the runtime should stop after a rejected initialize; error was: {error}"
    );
    let mut agent = start_mock_session(&handle, SessionId::new());
    let event = next_agent_update(&mut agent).await;
    let AgentUpdate::State(AttachmentState::Failed(message)) = &event else {
        panic!("expected the rejection to fan out as an error, got {event:?}");
    };
    assert!(
        message.contains("rejected the handshake")
            && message.contains("horizon ext version mismatch"),
        "error was: {message}"
    );
}

/// A peer that never answers `initialize` (a daemon generation that does
/// not speak ACP) escalates after `SILENCE_MISMATCH_THRESHOLD` silences to
/// the one recovery attempt, which dials a fresh ACP connection. The peer
/// cannot answer that either, so the runtime reports the manual fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silent_daemon_is_reported_as_needing_a_manual_stop() {
    std::env::set_var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS", "300");
    let (socket_path, control_socket) = stub_socket_paths("probe");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);
    let mut agent = start_mock_session(&handle, SessionId::new());

    let mut held = Vec::new();
    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }

    // Connection 4: the recovery, an ACP `initialize` line.
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        let bytes = read_until_closed(&mut stream).await;
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.starts_with('{') && text.contains("\"initialize\""),
            "the recovery must dial ACP; got {text:?}"
        );
    }

    let event = next_agent_update(&mut agent).await;
    let AgentUpdate::State(AttachmentState::Failed(message)) = &event else {
        panic!("expected the unrecoverable mismatch to fan out as an error, got {event:?}");
    };
    assert!(message.contains("stop it manually"), "error was: {message}");

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// Recovery is attempted exactly once per runtime: a replacement daemon
/// that is just as silent makes the runtime fail loudly with the rebuild
/// hint instead of draining again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_generation_mismatch_after_recovery_goes_fatal_instead_of_looping() {
    std::env::set_var("HORIZON_TEST_ESTABLISH_TIMEOUT_MS", "300");
    let (socket_path, control_socket) = stub_socket_paths("fatal");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);
    let mut agent = start_mock_session(&handle, SessionId::new());

    let mut held = Vec::new();
    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }
    {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_until_closed(&mut stream).await;
    }
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(&socket_path);

    for _ in 0..3 {
        let (stream, _) = listener.accept().await.unwrap();
        held.push(hold_silently(stream));
    }

    let event = next_agent_update(&mut agent).await;
    let AgentUpdate::State(AttachmentState::Failed(message)) = &event else {
        panic!("expected the fatal mismatch to fan out as an error, got {event:?}");
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

/// Serves `mismatched` on the stub's next connection and expects the
/// runtime's recovery there: `initialize`, then `_horizon/drain`.
async fn expect_drain(listener: &tokio::net::UnixListener) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut fake = serve_fake_agentd(
        stream,
        FakeAgentBehavior {
            mismatched_ext_version: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    assert!(matches!(fake.next_call().await, AgentCall::Drain));
}

/// Replaces the drained daemon with a compatible one and checks it is
/// adopted.
async fn expect_respawn_adopted(
    listener: tokio::net::UnixListener,
    socket_path: &std::path::Path,
    handle: &AgentdHandle,
) {
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listener = bind_stub_listener(socket_path);
    let (stream, _) = listener.accept().await.unwrap();
    let mut fake = serve_fake_agentd(stream, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));
}

/// A daemon that reports another extension version on a successful
/// `initialize` is drained on that same connection; the recovery's fresh
/// connection drains it again and the respawned daemon is adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_version_mismatched_daemon_is_drained_on_the_same_connection_and_the_respawn_adopted() {
    let (socket_path, control_socket) = stub_socket_paths("rej");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);

    expect_drain(&listener).await;
    expect_drain(&listener).await;
    expect_respawn_adopted(listener, &socket_path, &handle).await;

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// A request the daemon rejects with the extension-version mismatch error
/// enters the same recovery: drain on that connection, then respawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_rejected_for_the_extension_version_drains_and_respawns() {
    let (socket_path, control_socket) = stub_socket_paths("reqrej");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);

    let (stream, _) = listener.accept().await.unwrap();
    let mut rejecting = serve_fake_agentd(
        stream,
        FakeAgentBehavior {
            reject_requests: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(rejecting.next_call().await, AgentCall::Initialize));
    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert!(listed
        .unwrap()
        .unwrap_err()
        .contains("horizon ext version mismatch"));
    assert!(matches!(rejecting.next_call().await, AgentCall::List));
    assert!(matches!(rejecting.next_call().await, AgentCall::Drain));

    expect_drain(&listener).await;
    expect_respawn_adopted(listener, &socket_path, &handle).await;

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_transient_failures_do_not_consume_the_recovery_budget() {
    let (socket_path, control_socket) = stub_socket_paths("transient");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);

    for _ in 0..2 {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    }

    let (stream, _) = listener.accept().await.unwrap();
    let mut fake = serve_fake_agentd(stream, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

/// A connection dropping while `initialize` is in flight is a transient,
/// retried like any other pre-initialize drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_drop_during_initialize_is_retried_not_fatal() {
    let (socket_path, control_socket) = stub_socket_paths("initdrop");
    let listener = bind_stub_listener(&socket_path);
    let (handle, _host_tools, _workspace_roots) =
        AgentdHandle::start(&socket_path, &control_socket);

    let (stream, _) = listener.accept().await.unwrap();
    let hanging = serve_fake_agentd(
        stream,
        FakeAgentBehavior {
            hang_initialize: true,
            ..Default::default()
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    hanging.task.abort();

    let (stream, _) = listener.accept().await.unwrap();
    let mut fake = serve_fake_agentd(stream, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    let list_handle = handle.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));

    drop(handle);
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn organizer_request_uses_existing_agent_connection() {
    let (client, server) = pair();
    let (agentd, _host_tools, _workspace_roots) = AgentdHandle::start_on_stream(client);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    let root = PathBuf::from("/project/organizer");
    let expected_root = root.clone();
    let request = tokio::task::spawn_blocking(move || agentd.ensure_board_organizer(root));
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    let AgentCall::EnsureBoardOrganizer(root, id) = fake.next_call().await else {
        panic!("organizer request must reach the existing agent connection");
    };
    assert_eq!(root, expected_root);
    assert_eq!(request.await.unwrap().unwrap(), id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replacing_an_agent_handle_keeps_the_new_attachment_live() {
    let (client, server) = pair();
    let (agentd, _host, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let old = agentd.attach_session(id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    assert!(matches!(fake.next_call().await, AgentCall::Resume { .. }));
    let mut current = agentd.attach_session(id);
    let AgentCall::Resume { session_id, .. } = fake.next_call().await else {
        panic!("expected the replacement attachment");
    };
    assert_eq!(session_id, id);
    drop(old);
    wait_until_ready(&mut current).await;
    fake.update(id, idle());
    assert!(is_idle_update(&next_agent_event(&mut current).await));
    current.sender().send(AgentCommand::ContinueTurn).unwrap();
    assert!(matches!(fake.next_call().await, AgentCall::ContinueTurn));
}

/// Commands wait for the resume response; the replay before it reaches the
/// pane. A resume error fails the attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_commands_wait_for_the_resume_response_and_a_resume_error_fails() {
    let (client, server) = pair();
    let (agentd, _host, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = agentd.attach_session(id);
    let mut fake = serve_fake_agentd(
        server,
        FakeAgentBehavior {
            manual_bootstrap: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    let AgentCall::Resume { responder, .. } = fake.next_call().await else {
        panic!("resume");
    };
    handle
        .sender()
        .send(AgentCommand::Prompt {
            text: "queued".into(),
        })
        .unwrap();
    fake.update(id, idle());
    assert!(is_idle_update(&next_agent_event(&mut handle).await));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fake.calls.recv())
            .await
            .is_err(),
        "no command before the resume response"
    );
    responder
        .unwrap()
        .respond(v2::ResumeSessionResponse::new())
        .unwrap();
    let AgentCall::Prompt(text) = fake.next_call().await else {
        panic!("the queued prompt");
    };
    assert_eq!(text, "queued");

    let mut failing = agentd.attach_session(SessionId::new());
    let AgentCall::Resume { responder, .. } = fake.next_call().await else {
        panic!("resume");
    };
    responder
        .unwrap()
        .respond_with_error(agent_client_protocol::Error::new(-32602, "unknown session"))
        .unwrap();
    assert!(
        matches!(next_agent_update(&mut failing).await, AgentUpdate::State(AttachmentState::Failed(message)) if message.contains("unknown session"))
    );
}

/// Before the attachment's own resume response, a `Replaced` closes the
/// lease an earlier attachment held and is not this attachment's end; after
/// it, `Lagged` fails the attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_closed_notices_follow_the_resume_boundary() {
    let (client, server) = pair();
    let (agentd, _host, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = agentd.attach_session(id);
    let mut fake = serve_fake_agentd(
        server,
        FakeAgentBehavior {
            manual_bootstrap: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    let AgentCall::Resume { responder, .. } = fake.next_call().await else {
        panic!("resume");
    };
    let closed = |reason| SessionEventNotification::AttachmentClosed {
        session_id: id,
        reason,
    };
    fake.connection
        .send_notification(closed(AttachmentEnd::Replaced))
        .unwrap();
    responder
        .unwrap()
        .respond(v2::ResumeSessionResponse::new())
        .unwrap();
    wait_until_ready(&mut handle).await;
    fake.connection
        .send_notification(closed(AttachmentEnd::Lagged))
        .unwrap();
    assert!(
        matches!(next_agent_update(&mut handle).await, AgentUpdate::State(AttachmentState::Failed(message)) if message.contains("buffer"))
    );
}

/// A permission request is held by the session's attachment and answered
/// from the pane's decision; a deny carries its reason, a cancel answers
/// `Cancelled`, and a request for a session no pane holds is answered
/// `Cancelled` at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permission_requests_are_answered_from_the_panes_decisions() {
    let (client, server) = pair();
    let (agentd, _host, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = start_mock_session(&agentd, id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    assert!(matches!(
        fake.next_call().await,
        AgentCall::NewSession { .. }
    ));
    wait_until_ready(&mut handle).await;

    let identity = |occurrence: &str| ToolCallIdentity {
        call_id: "call-1".into(),
        occurrence_id: occurrence.into(),
    };
    let requested = |event: AgentEvent| match event {
        AgentEvent::PermissionRequested(permission) => permission.identity,
        event => panic!("expected a permission request, got {event:?}"),
    };
    let resolved = |event: AgentEvent| match event {
        AgentEvent::PermissionResolved { identity, decision } => (identity, decision),
        event => panic!("expected a resolution, got {event:?}"),
    };
    let selected = |response: v2::RequestPermissionResponse| match response.outcome {
        v2::RequestPermissionOutcome::Selected(selected) => {
            (selected.option_id.0.to_string(), selected.meta)
        }
        outcome => panic!("expected a selected option, got {outcome:?}"),
    };

    let answer = fake.permission(id, &identity("occ-1"));
    assert_eq!(
        requested(next_agent_event(&mut handle).await),
        identity("occ-1")
    );
    handle
        .sender()
        .send(AgentCommand::Approve {
            identity: identity("occ-1"),
        })
        .unwrap();
    let (option, _) = selected(answer.block_task().await.unwrap());
    assert_eq!(option, PERMISSION_OPTION_APPROVE);
    assert_eq!(
        resolved(next_agent_event(&mut handle).await),
        (identity("occ-1"), PermissionDecision::Approved)
    );

    let answer = fake.permission(id, &identity("occ-2"));
    requested(next_agent_event(&mut handle).await);
    handle
        .sender()
        .send(AgentCommand::Deny {
            identity: identity("occ-2"),
            reason: Some("not now".into()),
        })
        .unwrap();
    let (option, meta) = selected(answer.block_task().await.unwrap());
    assert_eq!(option, PERMISSION_OPTION_DENY);
    let meta: PermissionResponseMeta = read_horizon_meta(meta.as_ref()).unwrap().unwrap();
    assert_eq!(meta.reason.as_deref(), Some("not now"));
    assert_eq!(
        resolved(next_agent_event(&mut handle).await).1,
        PermissionDecision::Denied
    );

    let answer = fake.permission(id, &identity("occ-3"));
    requested(next_agent_event(&mut handle).await);
    handle.sender().send(AgentCommand::Cancel).unwrap();
    assert!(matches!(
        answer.block_task().await.unwrap().outcome,
        v2::RequestPermissionOutcome::Cancelled
    ));
    assert!(matches!(fake.next_call().await, AgentCall::Cancel));
    assert_eq!(
        resolved(next_agent_event(&mut handle).await).1,
        PermissionDecision::Cancelled
    );

    let orphan = fake.permission(SessionId::new(), &identity("occ-4"));
    assert!(matches!(
        orphan.block_task().await.unwrap().outcome,
        v2::RequestPermissionOutcome::Cancelled
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_and_close_reach_the_daemon_as_acp_requests() {
    let (client, server) = pair();
    let (agentd, _host, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = start_mock_session(&agentd, id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));
    assert!(matches!(
        fake.next_call().await,
        AgentCall::NewSession { .. }
    ));
    wait_until_ready(&mut handle).await;
    handle
        .sender()
        .send(AgentCommand::Prompt {
            text: "hello".into(),
        })
        .unwrap();
    let AgentCall::Prompt(text) = fake.next_call().await else {
        panic!("prompt");
    };
    assert_eq!(text, "hello");
    handle.sender().send(AgentCommand::Close).unwrap();
    assert!(matches!(fake.next_call().await, AgentCall::Close));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_tool_requests_are_answered_through_the_responder() {
    let (client, server) = pair();
    let (agentd, host_tools, _roots) = AgentdHandle::start_on_stream(client);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    // The runtime answers requests only once initialized.
    let listed = {
        let agentd = agentd.clone();
        tokio::task::spawn_blocking(move || agentd.session_list()).await
    };
    assert_eq!(listed.unwrap(), Ok(Vec::new()));
    assert!(matches!(fake.next_call().await, AgentCall::Initialize));

    let answer = fake.connection.send_request(HostToolRequest {
        request_id: "req-1".into(),
        tool_id: "workspace.snapshot".into(),
        input: serde_json::json!({}),
    });
    let request = tokio::task::spawn_blocking(move || {
        host_tools.recv_timeout(Duration::from_secs(5)).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(request.request_id, "req-1");
    agentd
        .responder()
        .respond_host_tool("req-1", serde_json::json!({"tabs": []}));
    let HostToolResponse { output } = answer.block_task().await.unwrap();
    assert_eq!(output, serde_json::json!({"tabs": []}));
}
