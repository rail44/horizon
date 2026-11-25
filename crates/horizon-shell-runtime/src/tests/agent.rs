//! Agent-runtime tests against a fake ACP v2 agent (`Agent.v2()`), served
//! over one end of a `UnixStream` pair or a stub listener -- the transport
//! production uses. The fake records every request and notification it
//! receives; tests drive updates, permission requests, and host-tool
//! requests through its connection handle.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::ConnectionTo as V2ConnectionTo;
use agent_client_protocol::v2::Responder;
use agent_client_protocol::v2::schema as v2;
use agent_client_protocol::{Agent, ByteStreams, Client};
use horizon_acp::{
    AttachmentEnd, HORIZON_ACP_EXT_VERSION, InitializeMeta, PERMISSION_OPTION_APPROVE,
    PERMISSION_OPTION_DENY, SessionEventNotification, SessionId, write_horizon_meta,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::super::attachment::acp_session_id;
use super::super::*;
use super::{
    FakeBehavior, TerminalCall, expect_terminald_handshake, next_terminal_call, recv_output,
    serve_fake_terminal_hub, spec,
};
use crate::agent_model::{AgentEvent, PermissionDecision, ToolCallIdentity};

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
    /// The directory `session/list` gives every session the fake named.
    listed: Option<&'static str>,
}

enum AgentCall {
    Initialize,
    NewSession {
        /// The id the fake named the session with.
        named: SessionId,
        /// The `mode` option's initial value.
        mode: Option<String>,
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
    let named_sessions: Arc<Mutex<Vec<SessionId>>> = Arc::default();
    let (named_by_new, named_for_list) = (named_sessions.clone(), named_sessions);
    let record = |calls: &Calls, call| {
        let _ = calls.send(call);
    };
    let (c1, c2, c3, c4, c5, c6, c7) = (
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
                // The agent names its sessions.
                let named = SessionId::new();
                named_by_new.lock().unwrap().push(named);
                let cwd = request.cwd.0.clone();
                let mode = (request.config_options.iter())
                    .find(|option| &*option.config_id.0 == horizon_acp::MODE_CONFIG_ID)
                    .and_then(|option| option.value.as_id())
                    .map(|value| value.0.to_string());
                if behavior.manual_bootstrap {
                    record(
                        &c2,
                        AgentCall::NewSession {
                            named,
                            mode,
                            cwd,
                            responder: Some(responder),
                        },
                    );
                    return Ok(());
                }
                let response = v2::NewSessionResponse::new(acp_session_id(named));
                record(
                    &c2,
                    AgentCall::NewSession {
                        named,
                        mode,
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
                let sessions = (behavior.listed.into_iter())
                    .flat_map(|cwd| {
                        (named_for_list.lock().unwrap().iter())
                            .map(|session| v2::SessionInfo::new(acp_session_id(*session), cwd))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                responder.respond(v2::ListSessionsResponse::new(sessions))
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
    ) -> agent_client_protocol::v2::SentRequest<v2::RequestPermissionResponse> {
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
        request.subject = Some(v2::RequestPermissionSubject::ToolCall(Box::new(
            v2::ToolCallPermissionSubject::new(v2::ToolCallUpdate::new(
                identity.occurrence_id.clone(),
            )),
        )));
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
    agentd.start_session(session_id, None, Some(PathBuf::from("/work/project")))
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
    let (agentd, _workspace_roots) = AgentdHandle::start_on_stream(agent_client);

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

    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let AgentCall::NewSession { mode, cwd, .. } = fake.next_call().await else {
        panic!("the agent spawn must land on agentd");
    };
    // No role: no mode option, and the agent's default model.
    assert_eq!(mode, None);
    assert_eq!(cwd, PathBuf::from("/work/project"));

    let output = TerminalUpdate::Output(b"terminal".to_vec());
    peer.events.send(output).await.unwrap();
    wait_until_ready(&mut agent).await;
    fake.update(agent_id, idle());
    terminal
        .sender()
        .send(TerminalCommand::Input(b"fifo".to_vec()))
        .unwrap();

    recv_output(terminal.events(), b"terminal").await;
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

/// `Reload Agent Runtime`'s client-runtime core: stopping the agent runtime
/// reaches only its own connection, and the terminal session keeps
/// streaming frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_the_agent_runtime_leaves_the_terminal_runtime_untouched() {
    let (terminal_client, terminal_server) = tokio::io::duplex(64 * 1024);
    let (agent_client, agent_server) = pair();
    let terminald = TerminaldHandle::start_on_stream(terminal_client);
    let (agentd, _workspace_roots) = AgentdHandle::start_on_stream(agent_client);

    let terminal = terminald.start_terminal(uuid::Uuid::new_v4(), spec());
    let (mut terminal_calls, _tconn, _tserve) =
        serve_fake_terminal_hub(terminal_server, FakeBehavior::default()).await;
    let mut fake = serve_fake_agentd(agent_server, FakeAgentBehavior::default()).await;
    expect_terminald_handshake(&mut terminal_calls).await;
    let TerminalCall::CreateTerminal { peer, .. } = next_terminal_call(&mut terminal_calls).await
    else {
        panic!("expected the create call");
    };
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);

    // A full round trip proves the runtime is established.
    let list_handle = agentd.clone();
    let listed = tokio::task::spawn_blocking(move || list_handle.session_list()).await;
    assert_eq!(listed.unwrap(), Ok(Vec::new()));
    std::assert_matches!(fake.next_call().await, AgentCall::List);

    let stopping = agentd.clone();
    tokio::task::spawn_blocking(move || stopping.stop_and_wait())
        .await
        .unwrap();

    let output = TerminalUpdate::Output(b"still alive".to_vec());
    peer.events.send(output).await.unwrap();
    recv_output(terminal.events(), b"still alive").await;
    assert!(
        terminal_calls.try_recv().is_err(),
        "stopping the agent runtime must not send anything to terminald"
    );
}

/// The agent names a session the shell starts, and a `session_info_update`
/// sends the shell to `session/list` for the session's directory: both reach
/// the runtime's news, in that order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_root_updates_reach_their_own_channel() {
    let (client, server) = pair();
    let (handle, news) = AgentdHandle::start_on_stream(client);
    let started = SessionId::new();
    let mut agent = start_mock_session(&handle, started);

    let mut fake = serve_fake_agentd(
        server,
        FakeAgentBehavior {
            listed: Some("/tmp/repo/.horizon/worktrees/abcd1234"),
            ..Default::default()
        },
    )
    .await;
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let AgentCall::NewSession {
        named: session_id, ..
    } = fake.next_call().await
    else {
        panic!("a new session");
    };
    wait_until_ready(&mut agent).await;
    // The agent named the session: the shell is told first.
    assert_eq!(
        news.recv_timeout(Duration::from_secs(5)).unwrap(),
        RuntimeNews::Named {
            was: started,
            is: session_id,
        }
    );
    assert_eq!(agent.session_id(), session_id);

    fake.update(
        session_id,
        v2::SessionUpdate::SessionInfoUpdate(v2::SessionInfoUpdate::new()),
    );
    std::assert_matches!(fake.next_call().await, AgentCall::List);
    assert_eq!(
        news.recv_timeout(Duration::from_secs(5))
            .expect("the listed directory"),
        RuntimeNews::Directory(session_id, "/tmp/repo/.horizon/worktrees/abcd1234".into())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_before_the_daemon_answers_cancels_the_runtime() {
    let (client, _server) = pair();
    let (handle, _workspace_roots) = AgentdHandle::start_on_stream(client);
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
    let (handle, _workspace_roots) = AgentdHandle::start_on_stream(client);
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
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let mut new_responder = None;
    let mut resume_responder = None;
    for _ in 0..2 {
        match fake.next_call().await {
            AgentCall::NewSession {
                named, responder, ..
            } => new_responder = Some((named, responder.unwrap())),
            AgentCall::Resume { responder, .. } => resume_responder = responder,
            call => panic!("unexpected {call:?}"),
        }
    }
    let (named, responder) = new_responder.unwrap();
    responder
        .respond(v2::NewSessionResponse::new(acp_session_id(named)))
        .unwrap();
    wait_until_ready(&mut ready).await;

    fake.task.abort();
    drop(resume_responder);

    std::assert_matches!(
        next_agent_update(&mut ready).await,
        AgentUpdate::State(AttachmentState::Disconnected(_))
    );
    std::assert_matches!(
        next_agent_update(&mut restoring).await,
        AgentUpdate::State(AttachmentState::Failed(_))
    );
    assert!(
        handle
            .session_list()
            .unwrap_err()
            .contains("runtime stopped")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_initialize_on_a_test_stream_is_a_terminal_failure() {
    let (client, server) = pair();
    let (handle, _workspace_roots) = AgentdHandle::start_on_stream(client);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replacing_an_agent_handle_keeps_the_new_attachment_live() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let old = agentd.attach_session(id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    std::assert_matches!(fake.next_call().await, AgentCall::Resume { .. });
    let mut current = agentd.attach_session(id);
    let AgentCall::Resume { session_id, .. } = fake.next_call().await else {
        panic!("expected the replacement attachment");
    };
    assert_eq!(session_id, id);
    drop(old);
    wait_until_ready(&mut current).await;
    fake.update(id, idle());
    assert!(is_idle_update(&next_agent_event(&mut current).await));
    current.sender().send(AgentCommand::Cancel).unwrap();
    std::assert_matches!(fake.next_call().await, AgentCall::Cancel);
}

/// Commands wait for the resume response; the replay before it reaches the
/// pane. A resume error fails the attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_commands_wait_for_the_resume_response_and_a_resume_error_fails() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
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
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
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
    std::assert_matches!(
        next_agent_update(&mut failing).await, AgentUpdate::State(AttachmentState::Failed(message)) if message.contains("unknown session")
    );
}

/// Before the attachment's own resume response, a `Replaced` closes the
/// lease an earlier attachment held and is not this attachment's end; after
/// it, `Lagged` fails the attachment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachment_closed_notices_follow_the_resume_boundary() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
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
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
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
    std::assert_matches!(
        next_agent_update(&mut handle).await, AgentUpdate::State(AttachmentState::Failed(message)) if message.contains("buffer")
    );
}

/// A permission request is held by the session's attachment and answered
/// from the pane's decision; a deny carries its reason, a cancel answers
/// `Cancelled`, and a request for a session no pane holds is answered
/// `Cancelled` at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permission_requests_are_answered_from_the_panes_decisions() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = start_mock_session(&agentd, id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let AgentCall::NewSession { named: id, .. } = fake.next_call().await else {
        panic!("a new session");
    };
    wait_until_ready(&mut handle).await;

    let identity = |occurrence: &str| ToolCallIdentity {
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
    let (option, _) = selected(answer.block_task().await.unwrap());
    assert_eq!(option, PERMISSION_OPTION_DENY);
    // The reason is the next message.
    let AgentCall::Prompt(text) = fake.next_call().await else {
        panic!("the reason as a prompt");
    };
    assert_eq!(text, "not now");
    assert_eq!(
        resolved(next_agent_event(&mut handle).await).1,
        PermissionDecision::Denied
    );

    let answer = fake.permission(id, &identity("occ-3"));
    requested(next_agent_event(&mut handle).await);
    handle.sender().send(AgentCommand::Cancel).unwrap();
    std::assert_matches!(
        answer.block_task().await.unwrap().outcome,
        v2::RequestPermissionOutcome::Cancelled(_)
    );
    std::assert_matches!(fake.next_call().await, AgentCall::Cancel);
    assert_eq!(
        resolved(next_agent_event(&mut handle).await).1,
        PermissionDecision::Cancelled
    );

    let orphan = fake.permission(SessionId::new(), &identity("occ-4"));
    std::assert_matches!(
        orphan.block_task().await.unwrap().outcome,
        v2::RequestPermissionOutcome::Cancelled(_)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_and_close_reach_the_daemon_as_acp_requests() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
    let id = SessionId::new();
    let mut handle = start_mock_session(&agentd, id);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let AgentCall::NewSession { .. } = fake.next_call().await else {
        panic!("a new session");
    };
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
    std::assert_matches!(fake.next_call().await, AgentCall::Close);
}

/// A subagent's permission request comes with its own session id: it is
/// shown in its parent's pane, as the subagent's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subagents_permission_request_is_shown_where_its_parent_is() {
    let (client, server) = pair();
    let (agentd, _roots) = AgentdHandle::start_on_stream(client);
    let parent = SessionId::new();
    let mut handle = start_mock_session(&agentd, parent);
    let mut fake = serve_fake_agentd(server, FakeAgentBehavior::default()).await;
    std::assert_matches!(fake.next_call().await, AgentCall::Initialize);
    let AgentCall::NewSession { named: parent, .. } = fake.next_call().await else {
        panic!("a new session");
    };
    wait_until_ready(&mut handle).await;

    let child = SessionId::new();
    fake.update(
        parent,
        v2::SessionUpdate::SubagentUpdate(
            v2::SubagentUpdate::new(acp_session_id(child))
                .state(v2::StateUpdate::Running(v2::RunningStateUpdate::new())),
        ),
    );
    std::assert_matches!(
        next_agent_event(&mut handle).await,
        AgentEvent::Update(update) if matches!(*update, v2::SessionUpdate::SubagentUpdate(_))
    );
    let _answer = fake.permission(
        child,
        &ToolCallIdentity {
            occurrence_id: "child-occ".into(),
        },
    );
    let AgentEvent::PermissionRequested(permission) = next_agent_event(&mut handle).await else {
        panic!("the child's request reaches the parent's pane");
    };
    assert_eq!(permission.identity.occurrence_id, "child-occ");
    assert_eq!(
        permission.task_session_id,
        Some(child.as_uuid().to_string())
    );
}
