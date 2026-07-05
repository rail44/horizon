//! End-to-end test against the real `horizon-agentd` binary (spawned via
//! `CARGO_BIN_EXE_horizon-agentd`, only available because this test lives in
//! the same package as the `[[bin]]` target) -- see
//! `docs/agent-runtime-split-design.md`'s step 2 deliverables.

use std::io::{BufRead, Read};
use std::path::PathBuf;
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
use horizon_agent::wire::{
    self, Control, Envelope, EnvelopeBody, Hello, HostToolRequest, HostToolResponse, SessionLoad,
    SessionNew, SessionSummary, CONTRACT_VERSION,
};
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// The env var `horizon-agentd`'s `main` reads to artificially delay its
/// event-log-read-plus-resume phase -- see that binary's own doc comment on
/// the constant of the same name. Test-only; never set outside this file.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_RESUME_DELAY_MS";

/// The env var `horizon-agentd`'s `main` reads to artificially delay its
/// background DuckDB rebuild task -- the DuckDB analogue of
/// [`TEST_RESUME_DELAY_MS_VAR`], letting a test prove `hello`/`session_list`
/// stay reachable while a slow rebuild is still running. Test-only; never
/// set outside this file.
const TEST_DUCKDB_REBUILD_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_DUCKDB_REBUILD_DELAY_MS";

/// Owns the spawned `horizon-agentd` child and its socket path; kills the
/// child and removes the socket file on drop so a failing assertion doesn't
/// leak either across test runs.
struct AgentdProcess {
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

impl AgentdProcess {
    /// Spawns `horizon-agentd` pointed at a throwaway event log path and a
    /// nonexistent config file -- **hermetic on purpose**: without this,
    /// the binary's own config loader (`horizon_agent::config::
    /// load_file_config`) falls back to this machine's real
    /// `~/.config/horizon/config.toml`, and step 3's startup persistence
    /// open (`spawn_resume_task`/`open_persistence` in `main.rs`) would then
    /// read/rebuild-from a real developer's (potentially large, potentially
    /// concurrently-locked) event log and DuckDB file. Every test gets its
    /// own fresh, empty log path so runs are fast, deterministic, and never
    /// touch real user data.
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

    /// Same as [`Self::spawn_at`], but additionally sets `horizon-agentd`'s
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
            "horizon-agentd-e2e-no-such-config-{}-{}.toml",
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
            "horizon-agentd-e2e-state-{}-{}.duckdb",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"));
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
        let child = command.spawn().expect("failed to spawn horizon-agentd");
        Self {
            child,
            socket_path,
            event_log_path,
            state_db_path,
            stderr_lines: None,
        }
    }

    /// Same as [`Self::spawn_at`], but additionally sets `horizon-agentd`'s
    /// test-only [`TEST_DUCKDB_REBUILD_DELAY_MS_VAR`] hook -- for proving
    /// the DuckDB rebuild (task 1 of the readiness fix) no longer sits on
    /// the resume-readiness path `hello`/`session_list` block on.
    fn spawn_at_with_duckdb_rebuild_delay(
        socket_path: PathBuf,
        event_log_path: PathBuf,
        rebuild_delay_ms: u64,
    ) -> Self {
        let missing_config_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let state_db_path = std::env::temp_dir().join(format!(
            "horizon-agentd-e2e-state-{}-{}.duckdb",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let child = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"))
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .env_remove(TEST_RESUME_DELAY_MS_VAR)
            .env(
                TEST_DUCKDB_REBUILD_DELAY_MS_VAR,
                rebuild_delay_ms.to_string(),
            )
            .spawn()
            .expect("failed to spawn horizon-agentd");
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
            "horizon-agentd-e2e-no-such-config-{}-{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut child = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"))
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .env_remove(TEST_RESUME_DELAY_MS_VAR)
            .env_remove(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn horizon-agentd");

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

impl Drop for AgentdProcess {
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

/// Writes a fixture event log directly at `path`, one session per
/// `(SessionId, Vec<Event>)` pair, via the same `WriterHandle`/`Appender`
/// machinery `horizon-agent`'s own event-log tests use -- for tests below
/// that need a specific pre-existing log (particular sessions in particular
/// terminal/live states) *before* `horizon-agentd` itself ever runs, rather
/// than building one up by talking to a live process. Every record gets
/// [`mock_provider_id`] as its provider id, matching what `send_session_new`
/// uses, so resumed sessions' `SessionSummary`s are directly comparable to
/// the ones the rest of this file already asserts against.
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
        let mut appender = Appender::new(writer.clone(), session_id, Some(mock_provider_id()));
        appender
            .append_provider_events(events.into_iter().map(ProviderEvent::from).collect())
            .expect("append fixture events");
    }
    writer.flush().expect("flush fixture events");
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

/// Regression test for the 2026-07 repeated-approval OOM incident: a banner
/// that didn't visibly react to a held `y` key re-sent `Approve` for the
/// same still-running `bash` call 134 times in 29 seconds, spawning 134
/// concurrent subprocesses and OOMing the machine. Sends 10 `ApproveToolCall`
/// commands for the exact same `call_id` back-to-back, without waiting for
/// any reply in between, and confirms the tool call started exactly once —
/// both in the events this connection observed and in the persisted event
/// log — regardless of how many duplicates arrived. This holds
/// deterministically (not just "usually", given the real subprocess's own
/// timing) because a session's commands are processed one at a time, in
/// order, on its own dedicated thread (`session::run_session`): the first
/// `Approve` folds `ToolCallStarted` synchronously before the second one is
/// even dequeued.
#[tokio::test]
async fn repeated_rapid_approve_of_the_same_call_starts_bash_exactly_once() {
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

    // 10 rapid duplicate approvals for the exact same call, sent before
    // waiting on any reply -- reproduces a banner keypress repeated while
    // the round trip is still in flight.
    for _ in 0..10 {
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
    }

    let events = collect_events_until(
        &mut reader,
        session_id,
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

    // Confirm the same holds in the persisted on-disk log, not just what
    // this connection happened to observe over the wire.
    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read(&agentd.event_log_path)
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

// --- step-3 trims restored: streaming preview + skipped-lines status -------
//
// `docs/agent-runtime-split-design.md`'s step-3 notes recorded two UX
// features lost in the split: the tool-call-argument streaming preview
// never crossed the wire (filtered out before forwarding), and agentd's
// startup event-log corruption diagnostics were only ever printed to its
// own stderr. Both are restored; these two tests prove it end to end over
// the real socket, not just at the crate's own in-process seam (see
// `crates/horizon-agent/src/tests.rs`'s
// `runtime_state_store_folds_tool_call_progress_but_excludes_it_from_the_jsonl_log`
// for that pre-existing in-process coverage).

/// The mock provider's `"streaming tool"` trigger emits a few ephemeral
/// `ProviderEvent::tool_call_progress` ticks before the real
/// `ToolCallRequested` -- these must still reach a connected client (now as
/// `Control::ToolCallProgress`, since `contract::Event` has no slot for
/// them) even though tool execution/policy mapping moved into agentd, and
/// they must never appear in the durable on-disk event log in any form.
#[tokio::test]
async fn streaming_tool_call_progress_reaches_the_client_but_never_the_event_log() {
    let agentd = AgentdProcess::spawn();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    let session_id = SessionId::new();
    send_session_new(&mut writer, session_id).await;
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "please use the streaming tool".to_string(),
            },
        ),
    )
    .await
    .unwrap();

    let mut progress_ticks = Vec::new();
    let mut saw_tool_call_requested = false;
    for _ in 0..200 {
        let envelope = wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("agentd should keep streaming events while the session is live");
        assert_eq!(envelope.session_id, Some(session_id));
        match envelope.body {
            EnvelopeBody::Control(Control::ToolCallProgress(progress)) => {
                progress_ticks.push(progress);
            }
            EnvelopeBody::Event(Event::ToolCallRequested(request)) => {
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
        "expected every mock streaming tick to reach the client as its own control message, \
         got: {progress_ticks:?}"
    );
    assert!(
        progress_ticks
            .windows(2)
            .all(|pair| pair[1].bytes >= pair[0].bytes),
        "byte counts should grow monotonically as the mock provider streams, got: {progress_ticks:?}"
    );

    // The writer flushes after every append (step 4's durability fix), but
    // appending itself is asynchronous -- `Appender::append_provider_events`
    // just enqueues onto the writer's own background thread, which is what
    // actually touches the file (see `WriterHandle::open`'s "Ordering
    // guarantee" doc comment) -- so the on-disk write can trail the wire
    // delivery this test just raced ahead on. Poll rather than read once,
    // waiting for the real tool call request to actually land before
    // asserting anything about the file's contents.
    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read(&agentd.event_log_path)
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

    let log_contents = std::fs::read_to_string(&agentd.event_log_path)
        .expect("event log should exist and be readable");
    assert!(
        !log_contents
            .to_ascii_lowercase()
            .contains("tool_call_progress"),
        "the persisted event log must never contain the ephemeral tool-call-progress preview, \
         got:\n{log_contents}"
    );
}

/// A corrupt line found during this process's own startup event-log read
/// must be reported to a connecting client once, as a dedicated
/// `Control::SkippedLines` message -- not just printed to agentd's stderr --
/// so Horizon's status bar (`agent_state_status`) can surface it. Sent
/// after `hello` (never blocking it -- see `main`'s bind-first ordering)
/// but, for a log this small, well before anything else would arrive on a
/// connection with no sessions in it, so the very next envelope after the
/// handshake is deterministically the one under test.
#[tokio::test]
async fn corrupt_event_log_lines_are_reported_to_the_client_once_per_connection() {
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
    std::fs::write(&event_log_path, "not valid json\n").expect("write corrupt fixture log");

    let agentd = AgentdProcess::spawn_at(socket_path, event_log_path);
    let (mut reader, _writer) = connect_and_handshake(&agentd.socket_path).await;

    let envelope = wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("agentd should report its startup diagnostics rather than close the connection");
    let EnvelopeBody::Control(Control::SkippedLines(summary)) = envelope.body else {
        panic!(
            "expected a SkippedLines control message, got {:?}",
            envelope.body
        );
    };
    assert_eq!(summary, "skipped 1 corrupt line");
}

// --- step 4: replay, reconnect, session_load --------------------------------

async fn send_session_load(writer: &mut OwnedWriteHalf, session_id: SessionId) {
    wire::write_envelope(
        writer,
        &Envelope::control(Control::SessionLoad(SessionLoad { session_id })),
    )
    .await
    .unwrap();
}

/// Reads envelopes scoped to `session_id` until none arrive for a while,
/// returning them in order -- used for `session_load`'s replay burst, which
/// (unlike `collect_events_until`'s callers) has no single terminal event to
/// watch for: the reply is just "however many committed events this session
/// has", full stop. A generous quiet window (bigger than a same-host
/// loopback round trip needs) distinguishes "done replaying" from "still
/// coming".
async fn collect_replayed_events(
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: SessionId,
) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(500), wire::read_envelope(reader)).await {
            Ok(Ok(Some(envelope))) => {
                assert_eq!(envelope.session_id, Some(session_id));
                let EnvelopeBody::Event(event) = envelope.body else {
                    panic!("expected an event envelope, got {:?}", envelope.body);
                };
                events.push(event);
            }
            Ok(Ok(None)) => panic!("connection closed while collecting replayed events"),
            Ok(Err(err)) => panic!("wire error while collecting replayed events: {err}"),
            Err(_timeout) => return events,
        }
    }
}

/// Step 4's headline scenario: `kill -9` a live `horizon-agentd` mid-session
/// (while a turn is genuinely still open -- parked in `WaitingForApproval`
/// with no timer to close it on its own), respawn a fresh process pointed at
/// the same event log, and confirm replay does what the design promises:
/// the transcript survives, the interrupted turn is committed as cancelled
/// rather than left dangling, the session is immediately usable again
/// (listed by `session_list`, answers a fresh `session_load`), and a new
/// user message works normally.
#[tokio::test]
async fn killed_agentd_respawns_and_replays_transcript_with_open_turn_cancelled() {
    let agentd = AgentdProcess::spawn();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let (mut reader, mut writer) = connect_and_handshake(&socket_path).await;

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

    // Parks the session in `WaitingForApproval` indefinitely -- a genuinely
    // open turn, not a race against a timer -- once this arrives.
    collect_events_until(&mut reader, session_id, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;

    agentd.kill_and_wait();

    // A fresh process, pointed at the same socket and event log paths --
    // simulating a real restart (e.g. after a crash, or the binary being
    // rebuilt), not a clean shutdown.
    let respawned = AgentdProcess::spawn_at(socket_path, event_log_path);
    let (mut reader, mut writer) = connect_and_handshake(&respawned.socket_path).await;

    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
        }])),
        "the resumed session must be listed as live again"
    );

    send_session_load(&mut writer, session_id).await;
    let replayed = collect_replayed_events(&mut reader, session_id).await;

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

    // The session is genuinely live, not just listed: a fresh message works.
    wire::write_envelope(
        &mut writer,
        &Envelope::command(
            session_id,
            AgentCommand::UserMessage {
                text: "hello again".to_string(),
            },
        ),
    )
    .await
    .unwrap();
    let events = collect_events_until(&mut reader, session_id, |event| {
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

/// `session_load` bootstrap (no crash involved this time): a client that
/// disconnects and reconnects to the *same* running `horizon-agentd` must
/// see the session's frame come back identical to the one it had live,
/// proving `session_load`'s replayed events are genuinely fold-equivalent
/// to the events the client already saw -- not just "some events".
#[tokio::test]
async fn session_load_after_reconnect_rebuilds_an_equivalent_frame() {
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

    // Waits for the turn's actual closing event (`WaitingForUser` *after*
    // the reply -- the session emits it a couple of other times during its
    // own startup noise too), not just the assistant's reply that precedes
    // it: otherwise this frame would be missing the final state transition
    // `session_load`'s replay (read after the whole turn has long since
    // committed) always includes, comparing two frames that were never
    // really "the same point in time".
    let mut seen_reply = false;
    let live_events = collect_events_until(&mut reader, session_id, |event| {
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

    // Disconnect (drop both halves) without draining agentd -- the session
    // keeps running; only this client's connection goes away.
    drop(reader);
    drop(writer);

    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;
    send_session_load(&mut writer, session_id).await;
    let replayed_events = collect_replayed_events(&mut reader, session_id).await;
    let replayed_frame = agent_frame_from_events(&replayed_events);

    assert_eq!(
        replayed_frame, live_frame,
        "session_load's replay must fold to the exact same frame the live connection saw"
    );
}

/// The server-side substance of the `Reload Agent Runtime` command
/// (`agent::agentd_runtime::reload_agent_runtime` on the Horizon side, not
/// exercisable from this crate's tests -- `CARGO_BIN_EXE_horizon-agentd` is
/// only set for *this* package's own integration tests, per step 2's
/// notes): drain a live session gracefully (not a crash this time), respawn
/// against the same paths, and confirm the session survives with its
/// transcript intact and ready for more traffic -- the same guarantee
/// `killed_agentd_respawns_and_replays_transcript_with_open_turn_cancelled`
/// proves for a hard kill, proven here for the clean-shutdown path the
/// reload command actually drives.
#[tokio::test]
async fn drained_agentd_respawns_and_preserves_a_completed_session() {
    let mut agentd = AgentdProcess::spawn();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let (mut reader, mut writer) = connect_and_handshake(&socket_path).await;

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
    collect_events_until(&mut reader, session_id, |event| {
        matches!(
            event,
            Event::MessageCommitted(message)
                if message.role == MessageRole::Assistant && message.text == "Mock response: hello"
        )
    })
    .await;

    wire::write_envelope(&mut writer, &Envelope::control(Control::Drain))
        .await
        .unwrap();
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(status.success(), "drain should exit 0, got {status:?}");

    let respawned = AgentdProcess::spawn_at(socket_path, event_log_path);
    let (mut reader, mut writer) = connect_and_handshake(&respawned.socket_path).await;

    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(vec![SessionSummary {
            session_id,
            provider_id: mock_provider_id(),
        }])),
        "a gracefully drained session must resume too, not just a crashed one"
    );

    send_session_load(&mut writer, session_id).await;
    let replayed = collect_replayed_events(&mut reader, session_id).await;
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

// --- bind-first startup ordering + dead-session resume filter ---------------
//
// Regression coverage for a real startup failure: `horizon-agentd` used to
// read its event log and resume every session it found *before* binding the
// socket. On a big log this took long enough that Horizon's connect-retry
// budget timed out, concluded nothing was listening, and spawned a second
// `horizon-agentd` -- which itself replayed the whole log again before
// discovering the first instance already owned the socket. Separately,
// every session ever created (including long-dead ones) was being resumed
// on every restart, growing startup cost with history forever.

/// Fix 2: a session whose log already ends in a terminal state (`Terminated`
/// or an `Exited` item) must not be resumed -- only a session with no such
/// terminal marker at its tail should show up in `session_list` after
/// startup.
#[tokio::test]
async fn resume_skips_sessions_whose_log_already_ended_in_a_terminal_state() {
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

    let agentd = AgentdProcess::spawn_at(socket_path, event_log_path);
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;

    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
        }])),
        "only the live session should have been resumed, got {reply:?}"
    );
}

/// Fix 1: `hello` must answer well before a slow resume finishes, and
/// `session_list` must wait for it rather than racing it -- proven here with
/// the test-only resume-delay hook rather than relying on incidental timing,
/// since a normal (empty or tiny) fixture log resumes too fast to
/// distinguish "answered immediately" from "answered after a fast resume".
#[tokio::test]
async fn hello_answers_immediately_while_session_list_waits_for_a_slow_resume() {
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
    let agentd = AgentdProcess::spawn_at_with_resume_delay(
        socket_path,
        event_log_path,
        Some(RESUME_DELAY_MS),
    );

    let hello_started = Instant::now();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;
    let hello_elapsed = hello_started.elapsed();
    assert!(
        hello_elapsed < Duration::from_millis(RESUME_DELAY_MS / 2),
        "hello should answer well before the artificial resume delay elapses, took {hello_elapsed:?}"
    );

    let session_list_started = Instant::now();
    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    let session_list_elapsed = session_list_started.elapsed();
    assert!(
        session_list_elapsed >= Duration::from_millis(RESUME_DELAY_MS) - Duration::from_millis(300),
        "session_list should have waited for the (artificially slow) resume to finish, \
         took {session_list_elapsed:?}"
    );
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
        }])),
    );
}

/// Fix 1's other half: a second `horizon-agentd` pointed at a socket path a
/// live instance already owns must detect that and exit *before* it ever
/// reads its own event log -- proven by asserting the second instance's
/// stderr never mentions resuming a session, not just that it eventually
/// exits non-zero (which the old, wrongly-ordered code also did, just after
/// wastefully replaying the whole log first).
#[tokio::test]
async fn second_agentd_against_a_live_socket_exits_before_reading_its_own_log() {
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

    let first = AgentdProcess::spawn_at(socket_path.clone(), event_log_path.clone());
    // Wait for the first instance to be up and to have finished resuming
    // (via `SessionList`'s own readiness gate) before racing a second one
    // against it, so this test is only exercising the "socket already
    // live" path, not an unrelated bind race between the two.
    let (mut reader, mut writer) = connect_and_handshake(&first.socket_path).await;
    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    wire::read_envelope(&mut reader).await.unwrap().unwrap();
    drop(reader);
    drop(writer);

    let missing_config_path = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-no-such-config-{}-{}.toml",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    // Same hermeticity fix as `AgentdProcess::spawn_at_with_resume_delay`:
    // an unset `HORIZON_AGENT_STATE_DB` now resolves to a real default path
    // rather than "no DuckDB", so point it at its own throwaway path too --
    // though this instance is expected to bail (see below) before ever
    // reaching the code that would open it.
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-state-{}-{}.duckdb",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut second = Command::new(env!("CARGO_BIN_EXE_horizon-agentd"))
        .arg("--socket")
        .arg(&socket_path)
        .env("HORIZON_CONFIG", &missing_config_path)
        .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
        .env("HORIZON_AGENT_STATE_DB", &state_db_path)
        .env_remove(TEST_RESUME_DELAY_MS_VAR)
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn second horizon-agentd");

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

// --- DuckDB rebuild off the readiness path + skip-when-current -------------
//
// Regression coverage for the two other diagnosed causes of a slow-feeling
// `Reload Agent Runtime`/restart: the DuckDB projection rebuild used to run
// synchronously *before* readiness (`hello`/`session_list`/`session_new`
// all waited on it), and it always ran a full rebuild even when the log
// hadn't changed since the projection was last built.

/// Task 1: `hello`/`session_list` must both answer promptly even while an
/// (artificially slowed) DuckDB rebuild is still running in its own
/// background task -- proven with the same delay-hook shape
/// `hello_answers_immediately_while_session_list_waits_for_a_slow_resume`
/// uses for the resume phase, applied to the DuckDB rebuild instead.
#[tokio::test]
async fn duckdb_rebuild_delay_does_not_block_hello_or_session_list() {
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
    let agentd = AgentdProcess::spawn_at_with_duckdb_rebuild_delay(
        socket_path,
        event_log_path,
        REBUILD_DELAY_MS,
    );

    let hello_started = Instant::now();
    let (mut reader, mut writer) = connect_and_handshake(&agentd.socket_path).await;
    let hello_elapsed = hello_started.elapsed();
    assert!(
        hello_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "hello should answer well before the artificial duckdb rebuild delay elapses, \
         took {hello_elapsed:?}"
    );

    let session_list_started = Instant::now();
    wire::write_envelope(&mut writer, &Envelope::control(Control::SessionList))
        .await
        .unwrap();
    let reply = wire::read_envelope(&mut reader).await.unwrap().unwrap();
    let session_list_elapsed = session_list_started.elapsed();
    assert!(
        session_list_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "session_list must not wait on the (slow) duckdb rebuild, took {session_list_elapsed:?}"
    );
    assert_eq!(
        reply.body,
        EnvelopeBody::Control(Control::SessionListResult(vec![SessionSummary {
            session_id: live_session,
            provider_id: mock_provider_id(),
        }])),
    );
}

/// Task 2's skip path: a second spawn against an *unchanged* event log must
/// skip the DuckDB rebuild entirely once the freshness check finds the
/// existing projection's high-water mark already matches the log's tail --
/// observed directly via the "already current, skipping rebuild" stderr
/// marker `main::rebuild_duckdb_projection` logs, polled for while the
/// process is still alive (there's no over-the-wire signal for this: task
/// 1's whole point is that nothing waits on it).
///
/// The fixture's session must already be terminated: a *live* resumed
/// session's own thread replays its startup burst (`Created`/init-message/
/// `WaitingForUser`, per `session::resume_persisted_sessions`'s doc
/// comment) and persists it like any other event, which would keep growing
/// the log across every restart and make "unchanged" impossible to set up
/// at all. A terminated session is skipped by `resume_persisted_sessions`
/// entirely (see `session_is_dead`), so nothing appends to the log just
/// from starting `horizon-agentd` -- exactly the genuinely-static-log case
/// the skip optimization targets.
#[tokio::test]
async fn unchanged_log_skips_duckdb_rebuild_on_respawn() {
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
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-state-{}-{}.duckdb",
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

    let first = AgentdProcess::spawn_at_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    connect_and_handshake(&first.socket_path).await;
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

    let second =
        AgentdProcess::spawn_at_with_duckdb_options(socket_path, event_log_path, state_db_path);
    connect_and_handshake(&second.socket_path).await;
    second
        .wait_for_stderr_line("DuckDB projection already current, skipping rebuild")
        .await;
}

/// Task 2's other half: a log that grew (or otherwise changed) since the
/// projection was last built must still trigger a full rebuild -- the skip
/// optimization must never cause stale data to look "current".
#[tokio::test]
async fn stale_log_triggers_duckdb_rebuild_on_respawn() {
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
    let state_db_path = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-state-{}-{}.duckdb",
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

    let first = AgentdProcess::spawn_at_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    connect_and_handshake(&first.socket_path).await;
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

    // Append a new session to the *same* log file while agentd is down --
    // advances the log's tail sequence past what the projection recorded.
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
        AgentdProcess::spawn_at_with_duckdb_options(socket_path, event_log_path, state_db_path);
    connect_and_handshake(&second.socket_path).await;
    let rebuilt_line = second
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    assert!(
        !rebuilt_line.contains("skipping"),
        "a stale (grown) log must trigger a fresh rebuild, not the skip path: {rebuilt_line}"
    );
}
