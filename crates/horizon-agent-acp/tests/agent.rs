//! How Horizon's agent behaves over ACP, composed in this test's own
//! process (`horizon_agent_acp::Agent`) with places of the test's own, and
//! spoken to over an in-memory stream: the same ACP messages as over the
//! binary's standard input and output, with no binary to build and no
//! process to start (`docs/agent-stdio-child-design.md`, Tests). What is
//! about the process itself is `tests/e2e.rs`'s.

mod support;
use support::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema as v2;
use horizon_agent::contract::{Event, Exit, SessionId, SessionState};
use horizon_agent_acp::{Agent, Composition, Places, serve_until};
use horizon_agent_host::options as acp;
use horizon_daemon_testkit::{AcpClient, Inbound};

/// How a test composes the agent, past the places of its own.
#[derive(Default)]
struct Setup {
    config: horizon_config::RawConfig,
    /// The file the configuration is read from again when it changes.
    config_path: Option<PathBuf>,
    /// The session logs, where the test wrote some first.
    sessions_dir: Option<PathBuf>,
    control_socket: Option<PathBuf>,
    before_resume: Option<Box<dyn FnOnce() + Send>>,
    hold_mock_streams: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Setup {
    /// With the configuration in `path`, read again when it changes.
    fn configured(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("read the configuration");
        Self {
            config: horizon_config::parse(&text).expect("a configuration"),
            config_path: Some(path.to_path_buf()),
            ..Self::default()
        }
    }
}

/// Waits until `gate` is there.
fn wait_for(gate: &Path) {
    while !gate.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Holds the startup until `gate` is there.
fn waiting_on(gate: &Path) -> Box<dyn FnOnce() + Send> {
    let gate = gate.to_path_buf();
    Box::new(move || wait_for(&gate))
}

/// Holds the mock provider's slow turns, once their first word is out,
/// until `gate` is there.
fn holding_on(gate: &Path) -> Arc<dyn Fn() + Send + Sync> {
    let gate = gate.to_path_buf();
    Arc::new(move || wait_for(&gate))
}

/// The agent, composed in this process at places under a directory of the
/// test's own.
struct InProcess {
    agent: Agent,
    sessions_dir: PathBuf,
    _dir: tempfile::TempDir,
}

impl InProcess {
    fn start() -> Self {
        Self::with(Setup::default())
    }

    fn with(setup: Setup) -> Self {
        // Contained calls run under the runtime the binary configures.
        horizon_agent::tools::sandbox::configure(Arc::new(
            horizon_sandbox::runtime::FenceRuntime::new(
                horizon_agent::tools::sandbox::fence_on_path(),
            ),
        ));
        let dir = tempfile::tempdir().expect("a directory for the test");
        let mut places = Places::under(dir.path());
        if let Some(sessions_dir) = setup.sessions_dir {
            places.sessions_dir = sessions_dir;
        }
        let sessions_dir = places.sessions_dir.clone();
        let agent = Agent::compose(Composition {
            config: setup.config,
            config_path: setup.config_path,
            places,
            start_dir: None,
            control_socket: setup.control_socket,
            checkpoints: false,
            before_resume: setup.before_resume,
            hold_mock_streams: setup.hold_mock_streams,
            refused: Box::new(|error| panic!("the agent refused to start: {error:#}")),
        });
        Self {
            agent,
            sessions_dir,
            _dir: dir,
        }
    }

    /// Connects a client over an in-memory stream and initializes it.
    async fn connect(&self) -> AcpClient {
        let (client, served) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(served);
        tokio::spawn(serve_until(
            read,
            write,
            self.agent.state().clone(),
            std::future::pending(),
        ));
        let (read, write) = tokio::io::split(client);
        horizon_daemon_testkit::connect_initialized(read, write, "test-client").await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_then_switch_updates_an_existing_session_and_reattach() {
    fn applied_value(inbound: &Inbound) -> Option<String> {
        match update(inbound)? {
            v2::SessionUpdate::ConfigOptionUpdate(update) => {
                update.config_options.iter().find_map(|option| {
                    match (&*option.config_id.0 == acp::MODEL_CONFIG_ID, &option.kind) {
                        (true, v2::SessionConfigKind::Select(select)) => {
                            Some(select.current_value.0.to_string())
                        }
                        _ => None,
                    }
                })
            }
            _ => None,
        }
    }
    async fn applied(client: &mut AcpClient, expected: &str) {
        let collected = collect_until(client, |inbound| {
            if let Some(message) = session_error(inbound) {
                panic!("model application failed: {message}");
            }
            applied_value(inbound).as_deref() == Some(expected)
        })
        .await;
        assert!(!collected.is_empty());
    }
    fn set_model(session_id: SessionId, value: &str) -> v2::SetSessionConfigOptionRequest {
        v2::SetSessionConfigOptionRequest::new(
            acp_id(session_id),
            acp::MODEL_CONFIG_ID,
            v2::SessionConfigOptionValue::id(value.to_string()),
        )
    }

    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let initial = r#"default_model = { provider = "initial", model = "before-model" }
[[providers]]
name = "initial"
api_key_env = "HORIZON_SWITCH_TEST_KEY"
"#;
    std::fs::write(&config_path, initial).unwrap();
    let agent_acp = InProcess::with(Setup::configured(&config_path));
    let mut client = agent_acp.connect().await;
    let session_id = open_session(
        &client,
        new_session_request(
            &horizon_agent::registry::named_rig_provider_id("initial").0,
            test_cwd(),
        ),
    )
    .await;
    // Ensure the provider thread has captured its initial config before reload.
    collect_until(&mut client, |inbound| idle_stop_reason(inbound).is_some()).await;

    std::fs::write(
        &config_path,
        format!(
            "{initial}\n[[providers]]\nname = \"added\"\nvendor = \"anthropic\"\n\
             api_key_env = \"HORIZON_SWITCH_TEST_KEY\"\n"
        ),
    )
    .unwrap();
    // The agent reads the file again once it has sat still: the new
    // provider can be chosen then.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while client
        .request(set_model(session_id, "added/after-model"))
        .await
        .is_err()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the changed config file is never read again"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    applied(&mut client, "added/after-model").await;

    client
        .request(set_model(session_id, "added/wire-model"))
        .await
        .unwrap();
    applied(&mut client, "added/wire-model").await;

    assert!(
        client
            .request(set_model(session_id, "missing/bad-model"))
            .await
            .is_err()
    );
    let replay = resume(&mut client, session_id).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|inbound| applied_value(inbound).as_deref() == Some("added/wire-model")),
        "a reattachment announces the applied model: {replay:?}"
    );
}

/// `session/new` -> `session/prompt` -> the resulting updates arrive in
/// the order the mock provider produced them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_agent_then_user_message_streams_events_in_order() {
    let agent_acp = InProcess::start();
    let mut client = agent_acp.connect().await;

    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "hello").await;

    let collected = collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: hello")
    })
    .await;
    let user_index = collected
        .iter()
        .position(|inbound| user_message(inbound).as_deref() == Some("hello"))
        .expect("the user message should have been committed");
    assert!(
        user_index < collected.len() - 1,
        "the assistant's reply must land after the user's message, got: {collected:?}"
    );
}

/// `session/close` is answered once the session has sent its last: nothing
/// of it follows the answer, and the agent no longer lists it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_session_sends_nothing_after_the_answer() {
    let agent_acp = InProcess::start();
    let mut client = agent_acp.connect().await;
    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "slow word word word").await;
    collect_until(&mut client, |inbound| update(inbound).is_some()).await;

    client.send_ordered(v2::CloseSessionRequest::new(acp_id(session_id)));
    let collected = collect_until(&mut client, |inbound| {
        matches!(inbound, Inbound::Replied(_))
    })
    .await;
    let Some(Inbound::Replied(result)) = collected.last() else {
        panic!("the close is answered");
    };
    assert!(result.is_ok(), "{result:?}");
    let after = tokio::time::timeout(
        Duration::from_millis(500),
        client.next(Duration::from_secs(5)),
    )
    .await;
    assert!(
        after.is_err(),
        "nothing follows the answer: {:?}",
        after.ok()
    );
    let sessions = list_sessions(&client).await;
    assert!(
        !sessions
            .iter()
            .any(|session| session.session_id == acp_id(session_id)),
        "{sessions:?}"
    );
}

/// `session/list` reflects a session created via `session/new` on the same
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_agents_reflects_live_sessions_after_new_agent() {
    let agent_acp = InProcess::start();
    let client = agent_acp.connect().await;

    let session_id = open_session(&client, session_new()).await;

    let sessions = list_sessions(&client).await;
    assert_listed(&sessions, session_id, Some(test_cwd()));
    assert_eq!(sessions[0].cwd, v2::AbsolutePath::new(test_cwd()));
}

/// `workspace_snapshot` runs in the agent, which asks the control plane of
/// the shell that started it (`HORIZON_SOCKET`) for the workspace, and the
/// answer is the finished tool call's output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_snapshot_asks_the_shells_control_plane() {
    let socket = horizon_daemon_testkit::scratch_socket("agent-e2e-control");
    let control = serve_workspace_once(&socket, serde_json::json!({ "tab_count": 1 }));
    let agent_acp = InProcess::with(Setup {
        control_socket: Some(socket.clone()),
        ..Setup::default()
    });
    let mut client = agent_acp.connect().await;

    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "please take a snapshot").await;

    let collected = collect_until(&mut client, |inbound| {
        finished_tool(inbound).is_some_and(|call| {
            call.raw_output
                .as_opt_ref()
                .flatten()
                .is_some_and(|output| output["tab_count"] == 1)
        })
    })
    .await;
    assert_eq!(control.join().unwrap(), "workspace");
    assert!(
        collected.iter().filter_map(tool_update).any(|call| {
            call.name == MaybeUndefined::Value("workspace_snapshot".to_string())
                && call.title == MaybeUndefined::Value("Snapshot".to_string())
                && has_status(call, v2::ToolCallStatus::Pending)
        }),
        "expected the tool call to have been requested too, got: {collected:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_round_trip_request_out_approve_in_result_event_out() {
    let agent_acp = InProcess::start();
    let mut client = agent_acp.connect().await;
    let (session_id, approval, collected) =
        approve_and_finish(&mut client, "please run a tool").await;
    let finished = collected.iter().rev().find_map(finished_tool).unwrap();
    assert_eq!(*finished.tool_call_id.0, *approval.occurrence_id);

    // The finished call survives a reattachment.
    let replay = resume(&mut client, session_id).await.unwrap();
    let replayed = replay
        .iter()
        .rev()
        .find_map(finished_tool)
        .expect("the finished call is replayed");
    assert_eq!(*replayed.tool_call_id.0, *approval.occurrence_id);
}

/// `bash` runs in the agent: approving a real `bash` tool call (a write
/// beside the workspace, asked for outside the sandbox) spawns an actual
/// subprocess there, and the result arrives back as the call's final
/// update.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_runs_in_the_agent_and_reports_its_result_over_the_wire() {
    let agent_acp = InProcess::start();
    let mut client = agent_acp.connect().await;
    let (_outer, repo) = nested_session_fixture();
    let (_, approval, collected) = approve_and_finish_in(
        &mut client,
        shared_session_in(repo),
        "please write beside the workspace",
    )
    .await;
    let call = collected.iter().rev().find_map(finished_tool).unwrap();
    assert_eq!(*call.tool_call_id.0, *approval.occurrence_id);
    let output = call.raw_output.as_opt_ref().flatten().unwrap();
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["output"], "agent-acp-bash-ok\n");
}

/// Regression test for the 2026-07 repeated-approval OOM incident: a call
/// approved again and again -- each reattachment re-asks a still-pending
/// approval, and every ask is answered -- must start exactly once, both in
/// the updates and the persisted log, because a session's commands are
/// processed one at a time on its own dedicated thread.
/// A resume's replay, from its bootstrap on, with every approval asked
/// meanwhile approved: a resume asks again for an approval still pending,
/// and one asked while the last poll was ending would otherwise stay
/// unanswered.
async fn resume_approving(client: &mut AcpClient, session_id: SessionId) -> Vec<Inbound> {
    client.send_ordered(
        v2::ResumeSessionRequest::new(acp_id(session_id), test_cwd())
            .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
    );
    let mut collected = Vec::new();
    for inbound in collect_until(client, |inbound| matches!(inbound, Inbound::Replied(_))).await {
        match inbound {
            Inbound::Permission(_, responder) => {
                let _ = responder.respond(approve());
            }
            inbound => collected.push(inbound),
        }
    }
    while let Ok(inbound) = client.inbox.try_recv() {
        if let Inbound::Permission(_, responder) = inbound {
            let _ = responder.respond(approve());
        }
    }
    let start = (collected.iter())
        .rposition(|inbound| {
            matches!(inbound, Inbound::Update(notification)
                if notification.session_id == acp_id(session_id)
                    && matches!(notification.update, v2::SessionUpdate::SessionInfoUpdate(_)))
        })
        .unwrap_or(0);
    collected.split_off(start)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_rapid_approve_of_the_same_call_starts_bash_exactly_once() {
    let agent_acp = InProcess::start();
    let mut client = agent_acp.connect().await;

    let (_outer, repo) = nested_session_fixture();
    let session_id = open_session(&client, shared_session_in(repo)).await;
    prompt(&client, session_id, "please write beside the workspace").await;
    let mut collected = collect_until(&mut client, |inbound| {
        matches!(inbound, Inbound::Permission(..))
    })
    .await;
    let (request, responder) = take_permission(&mut collected);
    let approval = approval_meta(&request);
    responder.respond(approve()).unwrap();

    for _ in 0..10 {
        client.send_ordered(
            v2::ResumeSessionRequest::new(acp_id(session_id), test_cwd())
                .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
        );
        let collected = collect_until(&mut client, |inbound| {
            matches!(inbound, Inbound::Replied(_))
        })
        .await;
        for inbound in collected {
            if let Inbound::Permission(_, responder) = inbound {
                let _ = responder.respond(approve());
            }
        }
        // Answer re-asks that arrive after the replay too.
        while let Ok(inbound) = client.inbox.try_recv() {
            if let Inbound::Permission(_, responder) = inbound {
                let _ = responder.respond(approve());
            }
        }
    }

    let occurrence = approval.occurrence_id.clone();
    let mut replay = Vec::new();
    for _ in 0..200 {
        replay = resume_approving(&mut client, session_id).await;
        if replay
            .iter()
            .filter_map(finished_tool)
            .any(|call| *call.tool_call_id.0 == *occurrence)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let started = replay
        .iter()
        .filter_map(tool_update)
        .filter(|call| {
            *call.tool_call_id.0 == *occurrence && has_status(call, v2::ToolCallStatus::InProgress)
        })
        .count();
    assert_eq!(
        started, 1,
        "repeated approvals must start the tool call exactly once, got: {replay:?}"
    );
    let finished = replay
        .iter()
        .filter_map(finished_tool)
        .filter(|call| *call.tool_call_id.0 == *occurrence)
        .count();
    assert_eq!(
        finished, 1,
        "a duplicate approval must never produce a second result, got: {replay:?}"
    );

    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read_all(&agent_acp.sessions_dir)
            .expect("the on-disk event log should parse cleanly");
        if candidate.records.iter().any(|record| {
            matches!(&record.event, Event::ToolCallFinished(result) if result.occurrence_id.0 == occurrence)
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
        .filter(|record| {
            matches!(&record.event, Event::ToolCallStarted(id) if id.occurrence_id.0 == occurrence)
        })
        .count();
    assert_eq!(
        logged_started_count, 1,
        "the persisted event log must contain exactly one ToolCallStarted for the call, got: {:?}",
        report.records
    );
}

/// Fix 2: a session whose log already ends in a terminal state must not be
/// resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_skips_sessions_whose_log_already_ended_in_a_terminal_state() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agent-acp-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let terminated_session = SessionId::new();
    let exited_session = SessionId::new();
    let live_session = SessionId::new();
    write_session_fixture(
        &sessions_dir,
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

    let agent_acp = InProcess::with(Setup {
        sessions_dir: Some(sessions_dir),
        ..Setup::default()
    });
    let client = agent_acp.connect().await;
    assert_listed(&list_sessions(&client).await, live_session, None);
}

/// Fix 1: `initialize` must answer well before a slow resume finishes, and
/// `session/list` must wait for it -- proven with the resume-delay hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_answers_immediately_while_session_list_waits_for_a_slow_resume() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agent-acp-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let live_session = SessionId::new();
    write_session_fixture(
        &sessions_dir,
        vec![(
            live_session,
            vec![
                Event::StateChanged(SessionState::Created),
                Event::StateChanged(SessionState::WaitingForUser),
            ],
        )],
    );

    let gate = gate_path("resume");
    let agent_acp = InProcess::with(Setup {
        sessions_dir: Some(sessions_dir),
        before_resume: Some(waiting_on(&gate)),
        ..Setup::default()
    });

    // `initialize` answers while the resume is held...
    let client = tokio::time::timeout(NEVER, agent_acp.connect())
        .await
        .expect("initialize answers while the resume is held");
    // ...and `session/list` does not: it answers once the resume is let go,
    // with what the resume found. (A late answer would slip past the
    // first wait; one in it is caught.)
    let listing = list_sessions(&client);
    tokio::pin!(listing);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut listing)
            .await
            .is_err(),
        "session/list answered while the resume was held"
    );
    open_gate(&gate);
    let sessions = tokio::time::timeout(NEVER, listing)
        .await
        .expect("session/list answers once the resume is let go");
    assert_listed(&sessions, live_session, None);
    let _ = std::fs::remove_file(&gate);
}

/// Reattach while deltas are being committed. The full update sequence,
/// including the replay/live join, must equal a subsequent quiescent replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_attachments_during_streaming_join_history_and_live_exactly_once() {
    fn is_chunk(inbound: &Inbound) -> bool {
        matches!(
            update(inbound),
            Some(v2::SessionUpdate::AgentMessageChunk(_))
        )
    }
    // The turn is held once its first word is out: every attachment below
    // but the last is made while it is surely streaming, and the last one
    // takes in the rest live once it is let go.
    let gate = gate_path("mock-stream");
    let agent_acp = InProcess::with(Setup {
        hold_mock_streams: Some(holding_on(&gate)),
        ..Setup::default()
    });
    let mut client = agent_acp.connect().await;
    let session = open_session(&client, session_new()).await;
    prompt(&client, session, "slow word word word").await;
    collect_until(&mut client, is_chunk).await;
    for _ in 0..4 {
        let history = resume(&mut client, session).await.unwrap();
        assert!(history.iter().any(is_chunk));
        assert!(
            history
                .iter()
                .rev()
                .find(|inbound| is_state(inbound))
                .is_none_or(|inbound| idle_stop_reason(inbound).is_none()),
            "the turn ended while it was held"
        );
    }
    let mut messages = resume(&mut client, session).await.unwrap();
    open_gate(&gate);
    let settled = |messages: &[Inbound]| {
        messages
            .iter()
            .rev()
            .find(|inbound| is_state(inbound))
            .is_some_and(|inbound| idle_stop_reason(inbound).is_some())
    };
    if !settled(&messages) {
        messages.extend(
            collect_until(&mut client, |inbound| idle_stop_reason(inbound).is_some()).await,
        );
    }
    assert!(
        !messages
            .iter()
            .any(|inbound| { idle_stop_reason(inbound) == Some(Some(v2::StopReason::Cancelled)) })
    );
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(
        messages.iter().map(wire).collect::<Vec<_>>(),
        replay.iter().map(wire).collect::<Vec<_>>(),
        "streaming attach must preserve the entire ordered sequence"
    );
    let _ = std::fs::remove_file(&gate);
}
