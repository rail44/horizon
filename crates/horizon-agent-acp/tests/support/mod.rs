//! What both test binaries speak to the agent with: the ACP requests and
//! the readings of what comes back, and the fixtures a test writes first.
//! Each binary uses some of it.
#![allow(dead_code, unused_imports)]

use agent_client_protocol::Error;
use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema as v2;
use horizon_agent::contract::{
    Event, Exit, MessageRole, ProviderEvent, ProviderId, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::persistence::event_log::{Appender, WriterHandle};
use horizon_agent_host::options as acp;
use horizon_daemon_testkit::{
    AcpClient, AgentdPaths, AgentdProcess, AgentdSpawn, Inbound, cargo_bin_exe_var,
    resolve_daemon_binary, wait_for_exit,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// How long an answer that must come may take: only tells "never" apart.
pub const NEVER: Duration = Duration::from_secs(30);

/// A file a held phase waits for, not there yet; [`open_gate`] makes it.
pub fn gate_path(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hzn-gate-{what}-{}", uuid::Uuid::new_v4().simple()))
}

/// Lets what waits on `gate` go on.
pub fn open_gate(gate: &Path) {
    std::fs::write(gate, "").expect("make the gate");
}

pub fn test_cwd() -> PathBuf {
    std::env::current_dir().expect("test cwd should be readable")
}

pub fn acp_id(session_id: SessionId) -> v2::SessionId {
    v2::SessionId::new(session_id.as_uuid().to_string())
}

pub fn mock_provider_id() -> ProviderId {
    ProviderId("builtin.agent.mock".to_string())
}

/// A `session/new` on `provider_id`, confined to `cwd`.
pub fn new_session_request(provider_id: &str, cwd: PathBuf) -> v2::NewSessionRequest {
    v2::NewSessionRequest::new(cwd).config_options(initial_options(provider_id))
}

/// The initial `model` option value, on `provider_id`.
pub fn initial_options(provider_id: &str) -> Vec<v2::InitialSessionConfigOption> {
    vec![v2::InitialSessionConfigOption::new(
        acp::MODEL_CONFIG_ID,
        v2::SessionConfigOptionValue::id(acp::encode_model_option_id(provider_id, "default")),
    )]
}

/// A mock-provider session in the test's cwd.
pub fn session_new() -> v2::NewSessionRequest {
    new_session_request(&mock_provider_id().0, test_cwd())
}

/// Opens a session: the id the agent named it with.
pub async fn open_session(client: &AcpClient, request: v2::NewSessionRequest) -> SessionId {
    let response = client
        .request(request)
        .await
        .expect("session/new should succeed");
    SessionId::from_uuid(response.session_id.0.parse().expect("a uuid session id"))
}

pub async fn prompt(client: &AcpClient, session_id: SessionId, text: &str) {
    client
        .request(v2::PromptRequest::new(
            acp_id(session_id),
            vec![text.to_string().into()],
        ))
        .await
        .expect("session/prompt should be accepted");
}

pub async fn list_sessions(client: &AcpClient) -> Vec<v2::SessionInfo> {
    client
        .request(v2::ListSessionsRequest::new())
        .await
        .expect("session/list should succeed")
        .sessions
}

/// The one listed session, checked against its expected id and, when
/// given, its directory.
pub fn assert_listed(sessions: &[v2::SessionInfo], session_id: SessionId, cwd: Option<PathBuf>) {
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].session_id, acp_id(session_id));
    if let Some(cwd) = cwd {
        assert_eq!(sessions[0].cwd.0, cwd);
    }
}

/// The directory `session/list` gives `session_id`.
pub async fn listed_root(client: &AcpClient, session_id: SessionId) -> PathBuf {
    (list_sessions(client).await.into_iter())
        .find(|info| info.session_id == acp_id(session_id))
        .expect("the session is listed")
        .cwd
        .0
}

/// Waits for `session_id` to work outside `from`: each `session_info_update`
/// sends the client to `session/list` for its directory.
pub async fn moved_root(
    client: &mut AcpClient,
    session_id: SessionId,
    from: &Path,
) -> (Vec<Inbound>, PathBuf) {
    let mut collected = Vec::new();
    loop {
        collected.extend(collect_until(client, |inbound| info_update(inbound, session_id)).await);
        let root = listed_root(client, session_id).await;
        if root != from {
            return (collected, root);
        }
    }
}

/// Reads the inbox until `predicate` matches, returning every message
/// observed (including the matching one) in arrival order.
pub async fn collect_until(
    client: &mut AcpClient,
    mut predicate: impl FnMut(&Inbound) -> bool,
) -> Vec<Inbound> {
    let mut collected = Vec::new();
    for _ in 0..4000 {
        let inbound = client.next(Duration::from_secs(120)).await;
        let done = predicate(&inbound);
        collected.push(inbound);
        if done {
            return collected;
        }
    }
    panic!("gave up waiting for the expected message; got: {collected:?}");
}

/// Resumes `session_id` from the start and returns its bootstrap: the
/// messages from the attachment's leading `config_option_update` /
/// `session_info_update` up to the answer (anything a replaced attachment
/// on this connection sent before it is left out).
pub async fn resume(client: &mut AcpClient, session_id: SessionId) -> Result<Vec<Inbound>, Error> {
    client.send_ordered(
        v2::ResumeSessionRequest::new(acp_id(session_id), test_cwd())
            .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
    );
    let mut collected =
        collect_until(client, |inbound| matches!(inbound, Inbound::Replied(_))).await;
    let Some(Inbound::Replied(result)) = collected.pop() else {
        unreachable!()
    };
    result?;
    let start = collected
        .iter()
        .rposition(|inbound| {
            matches!(inbound, Inbound::Update(notification)
                if notification.session_id == acp_id(session_id)
                    && matches!(notification.update, v2::SessionUpdate::SessionInfoUpdate(_)))
        })
        .expect("a bootstrap carries session_info_update");
    // The model's config option, when known, precedes it.
    let start = match start.checked_sub(1).map(|index| update(&collected[index])) {
        Some(Some(v2::SessionUpdate::ConfigOptionUpdate(_))) => start - 1,
        _ => start,
    };
    Ok(collected.split_off(start))
}

pub fn update(inbound: &Inbound) -> Option<&v2::SessionUpdate> {
    match inbound {
        Inbound::Update(notification) => Some(&notification.update),
        _ => None,
    }
}

pub fn content_text(content: &MaybeUndefined<Vec<v2::ContentBlock>>) -> String {
    match content {
        MaybeUndefined::Value(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                v2::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

pub fn agent_message(inbound: &Inbound) -> Option<String> {
    match update(inbound)? {
        v2::SessionUpdate::AgentMessage(message) => Some(content_text(&message.content)),
        _ => None,
    }
}

pub fn user_message(inbound: &Inbound) -> Option<String> {
    match update(inbound)? {
        v2::SessionUpdate::UserMessage(message) => Some(content_text(&message.content)),
        _ => None,
    }
}

pub fn tool_update(inbound: &Inbound) -> Option<&v2::ToolCallUpdate> {
    match update(inbound)? {
        v2::SessionUpdate::ToolCallUpdate(call) => Some(call),
        _ => None,
    }
}

pub fn has_status(call: &v2::ToolCallUpdate, status: v2::ToolCallStatus) -> bool {
    call.status == MaybeUndefined::Value(status)
}

/// A tool call's final update: the one whose status ends it.
pub fn finished_tool(inbound: &Inbound) -> Option<&v2::ToolCallUpdate> {
    let call = tool_update(inbound)?;
    [
        v2::ToolCallStatus::Completed,
        v2::ToolCallStatus::Failed,
        v2::ToolCallStatus::Cancelled,
    ]
    .into_iter()
    .any(|status| has_status(call, status))
    .then_some(call)
}

/// The name the tool call `id` was requested under.
pub fn tool_name(collected: &[Inbound], id: &v2::ToolCallId) -> Option<String> {
    (collected.iter().filter_map(tool_update))
        .filter(|call| call.tool_call_id == *id)
        .find_map(|call| call.name.as_opt_ref().flatten().cloned())
}

pub fn idle_stop_reason(inbound: &Inbound) -> Option<Option<v2::StopReason>> {
    match update(inbound)? {
        v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle)) => {
            Some(idle.stop_reason.clone())
        }
        _ => None,
    }
}

pub fn is_state(inbound: &Inbound) -> bool {
    matches!(update(inbound), Some(v2::SessionUpdate::StateUpdate(_)))
}

/// An error the agent reports: an error notice, or the error a failed
/// turn's stop reason carries.
pub fn session_error(inbound: &Inbound) -> Option<&str> {
    match update(inbound)? {
        v2::SessionUpdate::Notice(notice) if notice.severity == v2::NoticeSeverity::Error => {
            Some(&notice.title)
        }
        v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle)) => match &idle.stop_reason {
            Some(v2::StopReason::Error(stop)) => Some(
                stop.error
                    .as_ref()
                    .map_or("", |error| error.message.as_str()),
            ),
            _ => None,
        },
        _ => None,
    }
}

/// What a permission request asks about: the tool call it names.
pub struct Approval {
    pub occurrence_id: String,
}

pub fn approval_meta(request: &v2::RequestPermissionRequest) -> Approval {
    let Some(v2::RequestPermissionSubject::ToolCall(subject)) = &request.subject else {
        panic!("the permission names its tool call: {request:?}");
    };
    Approval {
        occurrence_id: subject.tool_call.tool_call_id.0.to_string(),
    }
}

pub fn take_permission(
    collected: &mut Vec<Inbound>,
) -> (
    v2::RequestPermissionRequest,
    agent_client_protocol::v2::Responder<v2::RequestPermissionResponse>,
) {
    let index = collected
        .iter()
        .position(|inbound| matches!(inbound, Inbound::Permission(..)))
        .expect("a permission request should have arrived");
    let Inbound::Permission(request, responder) = collected.remove(index) else {
        unreachable!()
    };
    (request, responder)
}

pub fn approve() -> v2::RequestPermissionResponse {
    v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Selected(
        v2::SelectedPermissionOutcome::new(acp::PERMISSION_OPTION_APPROVE),
    ))
}

/// Everything comparable a message carries, requests' responders aside.
pub fn wire(inbound: &Inbound) -> serde_json::Value {
    match inbound {
        Inbound::Update(notification) => serde_json::to_value(notification),
        Inbound::Permission(request, _) => serde_json::to_value(request),
        Inbound::Replied(_) => Ok(serde_json::Value::Null),
    }
    .unwrap()
}

pub fn run_fixture_git(dir: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    command.env("GIT_CONFIG_SYSTEM", "/dev/null");
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn isolated_session_fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("create isolated-session fixture repo");
    fixture_repository(repo.path());
    repo
}

/// A session's repository one level inside a directory of the test's own:
/// what a call writes beside the repository is then the test's to throw
/// away. The directory, and the repository in it.
pub fn nested_session_fixture() -> (tempfile::TempDir, PathBuf) {
    let outer = tempfile::tempdir().expect("create nested-session fixture");
    let repo = outer.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    fixture_repository(&repo);
    (outer, repo)
}

/// Makes `repo` a repository with one commit.
pub fn fixture_repository(repo: &Path) {
    run_fixture_git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    run_fixture_git(repo, &["add", "README.md"]);
    run_fixture_git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
}

/// Writes a fixture event log directly at `path`, one session per
/// `(SessionId, Vec<Event>)` pair, via the same `WriterHandle`/`Appender`
/// machinery `horizon-agent`'s own event-log tests use -- for tests below
/// that need a specific pre-existing log *before* `horizon-agentd` itself
/// ever runs. Every record gets [`mock_provider_id`] as its provider id.
pub fn write_session_fixture(path: &std::path::Path, sessions: Vec<(SessionId, Vec<Event>)>) {
    let writer = WriterHandle::open(path);
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
/// the JSONL-era note preserved on `killed_agentd...`: a client can
/// observe an event over the wire before it is durable, so kill-based tests
/// must wait for the disk write.
pub async fn wait_for_persisted_event(
    path: &std::path::Path,
    session_id: SessionId,
    mut predicate: impl FnMut(&Event) -> bool,
) {
    for _ in 0..200 {
        if let Ok(report) = horizon_agent::persistence::event_log::read_all(path)
            && report
                .records
                .iter()
                .any(|record| record.session_id == session_id && predicate(&record.event))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "gave up waiting for the expected event to reach disk at {}",
        path.display()
    );
}

/// Answers one control-plane connection at `socket`: the handshake, then
/// `snapshot` as the workspace. Returns what was asked.
pub fn serve_workspace_once(
    socket: &Path,
    snapshot: serde_json::Value,
) -> std::thread::JoinHandle<String> {
    use horizon_control::contract::{
        CONTROL_VERSION, Envelope, EnvelopeBody, HelloAck, Query, Workspace,
    };
    use horizon_control::wire;
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let hello = wire::read_envelope(&mut reader).unwrap().unwrap();
        let ack = EnvelopeBody::HelloAck(HelloAck {
            control_version: CONTROL_VERSION,
            binary_id: "e2e".into(),
            capabilities: vec!["workspace".into()],
        });
        wire::write_envelope(&mut writer, &Envelope::new(hello.id, ack)).unwrap();
        let query = wire::read_envelope(&mut reader).unwrap().unwrap();
        let EnvelopeBody::Query(Query { what }) = query.body else {
            panic!("a query, got {query:?}");
        };
        let reply = EnvelopeBody::Workspace(Workspace { snapshot });
        wire::write_envelope(&mut writer, &Envelope::new(query.id, reply)).unwrap();
        what
    })
}

/// Approval round trip: the approval goes out as
/// `session/request_permission`, the approving answer comes back, and
/// agentd runs the call and reports it as ordinary tool-call updates.
pub async fn approve_and_finish(
    client: &mut AcpClient,
    text: &str,
) -> (SessionId, Approval, Vec<Inbound>) {
    approve_and_finish_in(client, session_new(), text).await
}

/// [`approve_and_finish`] for a session opened with `request`.
pub async fn approve_and_finish_in(
    client: &mut AcpClient,
    request: v2::NewSessionRequest,
    text: &str,
) -> (SessionId, Approval, Vec<Inbound>) {
    let session_id = open_session(client, request).await;
    prompt(client, session_id, text).await;

    let mut collected =
        collect_until(client, |inbound| matches!(inbound, Inbound::Permission(..))).await;
    let (request, responder) = take_permission(&mut collected);
    let approval = approval_meta(&request);
    assert_eq!(request.session_id, acp_id(session_id));
    let requested = collected
        .iter()
        .filter_map(tool_update)
        .find(|call| {
            has_status(call, v2::ToolCallStatus::Pending)
                && *call.tool_call_id.0 == *approval.occurrence_id
        })
        .expect("tool request before approval");
    let _ = requested;

    responder.respond(approve()).unwrap();
    let occurrence = approval.occurrence_id.clone();
    let collected = collect_until(client, |inbound| {
        finished_tool(inbound).is_some_and(|call| *call.tool_call_id.0 == *occurrence)
    })
    .await;
    assert!(
        collected.iter().filter_map(tool_update).any(|call| {
            *call.tool_call_id.0 == *occurrence && has_status(call, v2::ToolCallStatus::InProgress)
        }),
        "approving should have started the tool call before finishing it, got: {collected:?}"
    );
    (session_id, approval, collected)
}

/// A session started in a plain checkout, the shape whose `bash` calls
/// run sandboxed without approval and ask to run outside it.
pub fn shared_session_in(cwd: PathBuf) -> v2::NewSessionRequest {
    new_session_request(&mock_provider_id().0, cwd)
}

/// Whether `inbound` is a `session_info_update` of `session_id`.
pub fn info_update(inbound: &Inbound, session_id: SessionId) -> bool {
    let Inbound::Update(notification) = inbound else {
        return false;
    };
    notification.session_id == acp_id(session_id)
        && matches!(notification.update, v2::SessionUpdate::SessionInfoUpdate(_))
}

pub fn fixture_git_output(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The user messages on `session`'s branch of the log, in order.
pub fn branch_user_messages(log: &Path, session: SessionId) -> Vec<String> {
    let records = horizon_agent::persistence::event_log::read_all(log)
        .unwrap()
        .records;
    horizon_agent::persistence::event_log::Log::new(records)
        .branch(session)
        .into_iter()
        .filter_map(|record| match record.event {
            Event::MessageCommitted(message) if message.role == MessageRole::User => {
                Some(message.text)
            }
            _ => None,
        })
        .collect()
}

/// One prompt answered, up to the session going idle again.
pub async fn prompt_and_settle(
    client: &mut AcpClient,
    session_id: SessionId,
    text: &str,
) -> Vec<Inbound> {
    prompt(client, session_id, text).await;
    let answer = format!("Mock response: {text}");
    let mut answered = false;
    collect_until(client, |inbound| {
        answered |= agent_message(inbound).as_deref() == Some(answer.as_str());
        answered && idle_stop_reason(inbound).is_some()
    })
    .await
}

/// The cursor announced before the user message `text`.
pub fn cursor_before_message(collected: &[Inbound], text: &str) -> v2::CursorId {
    let message_id = collected
        .iter()
        .find_map(|inbound| match update(inbound)? {
            v2::SessionUpdate::UserMessage(message) if content_text(&message.content) == text => {
                Some(message.message_id.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no user message {text:?} in {collected:?}"));
    collected
        .iter()
        .find_map(|inbound| match update(inbound)? {
            v2::SessionUpdate::CursorUpdate(cursor) => match &cursor.anchor {
                Some(v2::CursorAnchor::UserMessage(anchor)) if anchor.message_id == message_id => {
                    Some(cursor.cursor_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("no cursor before {text:?} in {collected:?}"))
}
