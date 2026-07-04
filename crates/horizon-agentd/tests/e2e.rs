//! End-to-end test against the real `horizon-agentd` binary (spawned via
//! `CARGO_BIN_EXE_horizon-agentd`, only available because this test lives in
//! the same package as the `[[bin]]` target) -- see
//! `docs/agent-runtime-split-design.md`'s step 2 deliverables.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use horizon_agent::contract::{Command as AgentCommand, Event, MessageRole, ProviderId, SessionId};
use horizon_agent::wire::{
    self, Control, Envelope, EnvelopeBody, Hello, HostToolRequest, HostToolResponse, SessionNew,
    SessionSummary, CONTRACT_VERSION,
};
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// Owns the spawned `horizon-agentd` child and its socket path; kills the
/// child and removes the socket file on drop so a failing assertion doesn't
/// leak either across test runs.
struct AgentdProcess {
    child: Child,
    socket_path: PathBuf,
    event_log_path: PathBuf,
}

impl AgentdProcess {
    /// Spawns `horizon-agentd` pointed at a throwaway event log path and a
    /// nonexistent config file -- **hermetic on purpose**: without this,
    /// the binary's own config loader (`horizon_agent::config::
    /// load_file_config`) falls back to this machine's real
    /// `~/.config/horizon/config.toml`, and step 3's eager startup
    /// persistence open (`build_agentd_state`/`open_persistence` in
    /// `main.rs`) would then read/rebuild-from a real developer's
    /// (potentially large, potentially concurrently-locked) event log and
    /// DuckDB file. Every test gets its own fresh, empty log path so runs
    /// are fast, deterministic, and never touch real user data.
    fn spawn() -> Self {
        let socket_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-{}-{}.sock",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let event_log_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-events-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let child = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"))
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env_remove("HORIZON_AGENT_STATE_DB")
            .spawn()
            .expect("failed to spawn horizon-agentd");
        Self {
            child,
            socket_path,
            event_log_path,
        }
    }
}

impl Drop for AgentdProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.event_log_path);
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
        "horizon-agentd never accepted a connection on {}",
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
    panic!("horizon-agentd did not exit in time");
}

/// Connects and completes the `hello` handshake, returning the split halves
/// ready for step 3's session-hosting traffic (`session_new`, commands,
/// events) -- every new test below needs this same sequence, so it's
/// factored out rather than repeated the way the two step 2 tests above
/// (which test the handshake itself) inline it.
async fn connect_and_handshake(
    socket_path: &std::path::Path,
) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = connect_with_retry(socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(Hello {
            contract_version: CONTRACT_VERSION,
            binary_id: "test-client".to_string(),
            capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();
    let reply = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should reply to hello");
    assert!(
        matches!(reply.body, EnvelopeBody::Control(Control::Hello(_))),
        "expected a hello reply, got {:?}",
        reply.body
    );

    (reader, write_half)
}

/// Reads envelopes until `predicate` matches one and returns every event
/// observed so far (including the matching one), in arrival order -- the
/// "streamed events arrive in order" / "transcript assertions" shape the
/// step 3 deliverables call for. Panics after a generous number of reads
/// rather than hanging forever if `predicate` never matches.
async fn collect_events_until(
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: SessionId,
    mut predicate: impl FnMut(&Event) -> bool,
) -> Vec<Event> {
    let mut events = Vec::new();
    for _ in 0..200 {
        let envelope = wire::read_envelope(reader)
            .await
            .unwrap()
            .expect("agentd should keep streaming events, not close the connection");
        assert_eq!(
            envelope.session_id,
            Some(session_id),
            "event envelope should be scoped to the session that produced it"
        );
        let EnvelopeBody::Event(event) = envelope.body else {
            panic!("expected an event envelope, got {:?}", envelope.body);
        };
        let done = predicate(&event);
        events.push(event);
        if done {
            return events;
        }
    }
    panic!("gave up waiting for the expected event after 200 reads; got: {events:?}");
}

#[tokio::test]
async fn hello_ping_session_list_and_drain_over_the_real_socket() {
    let mut agentd = AgentdProcess::spawn();
    let stream = connect_with_retry(&agentd.socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(Hello {
            contract_version: CONTRACT_VERSION,
            binary_id: "test-client".to_string(),
            capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();

    let reply = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should reply to hello");
    let EnvelopeBody::Control(Control::Hello(hello)) = reply.body else {
        panic!("expected a hello reply, got {:?}", reply.body);
    };
    assert_eq!(hello.contract_version, CONTRACT_VERSION);
    assert!(!hello.binary_id.is_empty());

    wire::write_envelope(&mut write_half, &Envelope::control(Control::Ping))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(reply.body, EnvelopeBody::Control(Control::Pong));

    wire::write_envelope(&mut write_half, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(Vec::new()))
    );

    wire::write_envelope(&mut write_half, &Envelope::control(Control::Drain))
        .await
        .unwrap();

    let status = wait_for_exit(&mut agentd.child).await;
    assert!(
        status.success(),
        "horizon-agentd should exit 0 after drain, got {status:?}"
    );
}

#[tokio::test]
async fn a_hello_with_the_wrong_contract_version_is_rejected_with_a_reason() {
    let agentd = AgentdProcess::spawn();
    let stream = connect_with_retry(&agentd.socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let wrong_version = CONTRACT_VERSION + 1;
    wire::write_envelope(
        &mut write_half,
        &Envelope::control(Control::Hello(Hello {
            contract_version: wrong_version,
            binary_id: "test-client".to_string(),
            capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();

    let reply = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should still answer, with a rejection");
    let EnvelopeBody::Control(Control::HandshakeRejected(reason)) = reply.body else {
        panic!("expected a handshake rejection, got {:?}", reply.body);
    };
    assert!(
        reason.contains("reload required"),
        "rejection reason was: {reason}"
    );

    // Rejected handshakes end the connection -- the next read observes a
    // clean close rather than the server continuing to serve requests for
    // a client whose contract version it can't trust.
    let next = wire::read_envelope(&mut reader).await.unwrap();
    assert!(next.is_none(), "expected the connection to be closed");
}

// --- step 3: session hosting -----------------------------------------------

fn mock_provider_id() -> ProviderId {
    ProviderId("builtin.agent.mock".to_string())
}

async fn send_session_new(writer: &mut OwnedWriteHalf, session_id: SessionId) {
    wire::write_envelope(
        writer,
        &Envelope::control(Control::SessionNew(SessionNew {
            session_id,
            provider_id: mock_provider_id(),
            config_overrides: None,
        })),
    )
    .await
    .unwrap();
}

/// Reads envelopes until a `Control::HostToolRequest` scoped to `session_id`
/// arrives, tolerating (and discarding) any event envelopes ahead of it --
/// agentd forwards the host tool's own `ToolCallRequested`/`ToolCallStarted`/
/// `ToolCallFinished` events only *after* the round trip completes (see
/// `session::handle_provider_event`), but earlier events in the same turn
/// (e.g. the triggering `StateChanged`/`MessageCommitted`) can arrive first.
async fn read_host_tool_request(
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: SessionId,
) -> HostToolRequest {
    for _ in 0..200 {
        let envelope = wire::read_envelope(reader)
            .await
            .unwrap()
            .expect("connection should stay open while a session is live");
        if let EnvelopeBody::Control(Control::HostToolRequest(request)) = envelope.body {
            assert_eq!(envelope.session_id, Some(session_id));
            return request;
        }
    }
    panic!("host_tool_request never arrived");
}

/// `session_new` -> `UserMessage` -> the resulting events arrive over the
/// wire in the same order the mock provider produced them, forming a
/// coherent transcript (the user's message, then the assistant's reply).
#[tokio::test]
async fn session_new_then_user_message_streams_events_in_order() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "hello".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let events = collect_events_until(&mut reader, session_id, |event| {
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

/// `session_list` reflects a session created via `session_new` on the same
/// connection -- agentd, not an empty stub (step 2's behavior).
#[tokio::test]
async fn session_list_reflects_live_sessions_after_session_new() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();

    // The session's own startup burst and the `SessionListResult` reply can
    // arrive in either order (one is produced by the freshly spawned
    // session thread, the other by the connection loop itself) -- skip past
    // any event envelopes to find the control reply.
    for _ in 0..200 {
        let envelope = wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("connection should stay open");
        if let EnvelopeBody::Control(Control::SessionListResult(sessions)) = envelope.body {
            assert_eq!(
                sessions,
                vec![SessionSummary {
                    session_id,
                    provider_id: mock_provider_id(),
                }]
            );
            return;
        }
    }
    panic!("SessionListResult never arrived");
}

/// An auto-allow *host* tool (`workspace.snapshot`) executes agentd-side but
/// can't answer itself -- it must round-trip a `host_tool_request` over the
/// connection (guardrail 4) and fold the client's `host_tool_response` into
/// the same `ToolCallFinished` event an ordinary auto tool would produce.
#[tokio::test]
async fn auto_tool_executes_agentd_side_via_host_tool_round_trip() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "please take a snapshot".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let request = read_host_tool_request(&mut reader, session_id).await;
    assert_eq!(request.tool_id, "workspace.snapshot");

    wire::write_envelope(
        &mut writer,
        &Envelope {
            v: CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Control(Control::HostToolResponse(HostToolResponse {
                request_id: request.request_id,
                output: serde_json::json!({ "tab_count": 1 }),
            })),
        },
    )
    .await
    .unwrap();

    let events = collect_events_until(
        &mut reader,
        session_id,
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
/// `ApproveToolCall` command flows back in, and agentd resolves it (decision
/// 2: "resolved in agentd") and reports the result as an ordinary event.
#[tokio::test]
async fn approval_round_trip_request_out_approve_in_result_event_out() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "please run a tool".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let events = collect_events_until(&mut reader, session_id, |event| {
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

    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::ApproveToolCall {
                call_id: call_id.clone(),
            },
        ),
    )
    .await
    .unwrap();

    let events = collect_events_until(
        &mut reader,
        session_id,
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

/// `bash` runs agentd-side: approving a real `bash` tool call spawns an
/// actual subprocess in agentd (via `tools::bash::spawn`, the same code
/// path Horizon used to run in-process) and the eventual result -- not just
/// the running-state events -- arrives back over the wire as an ordinary
/// event, proving the async completion (delivered internally on its own
/// channel, see `session::fold_bash_completion`) makes it out.
#[tokio::test]
async fn bash_runs_agentd_side_and_reports_its_result_over_the_wire() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "please run bash".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let events = collect_events_until(&mut reader, session_id, |event| {
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

    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::ApproveToolCall {
                call_id: call_id.clone(),
            },
        ),
    )
    .await
    .unwrap();

    // `ToolCallStarted` arrives synchronously with the approval; the result
    // arrives later, once the spawned process actually exits -- give it a
    // generous number of reads (`collect_events_until`'s cap) since this is
    // a real subprocess, not a synchronous fold.
    let events = collect_events_until(
        &mut reader,
        session_id,
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    )
    .await;

    let Some(Event::ToolCallFinished(result)) = events.iter().rev().find(
        |event| matches!(event, Event::ToolCallFinished(result) if result.call_id == call_id),
    ) else {
        panic!("expected a ToolCallFinished event for {call_id:?}, got: {events:?}");
    };
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "agentd-bash-ok\n");
}
