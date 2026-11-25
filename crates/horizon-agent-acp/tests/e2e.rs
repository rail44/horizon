//! End-to-end test against the real `horizon-agentd` binary, spawned
//! through `horizon-daemon-testkit`: it owns binary resolution (the runtime
//! `CARGO_BIN_EXE_horizon-agent-acp` env var in preference to the same-named
//! compile-time `env!()` bake -- `docs/tasks/backlog.md` #40), the
//! spawn-with-retry for cargo's uplift window (#36), and this daemon's
//! hermetic-spawn contract, which `horizon-terminald`'s suite spawns
//! through too -- see `docs/agent-runtime-split-design.md`'s step 2
//! deliverables.
//!
//! These talk to the daemon as an ACP v2 client on the actual unix socket
//! (`docs/acp-agentd-design.md`), through the testkit's [`AcpClient`]:
//! `initialize` with the extension version, `session/new`/`session/resume`
//! and the updates they stream, `session/request_permission` and
//! `_horizon/host_tool` round trips, and `_horizon/drain`.
//!
//! **Terminals are not here.** The v17 split
//! (`docs/terminald-split-design.md`) moved terminal hosting to
//! `horizon-terminald`, so its e2e coverage lives in
//! `crates/horizon-terminald/tests/e2e.rs`, which also owns the split's
//! acceptance test spawning *both* daemons.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_client_protocol::Error;
use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema as v2;
use horizon_acp as acp;
use horizon_agent::contract::{
    Event, Exit, MessageRole, ProviderEvent, ProviderId, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::persistence::event_log::{Appender, WriterHandle};
use horizon_daemon_testkit::{
    AcpClient, AgentdPaths, AgentdProcess, AgentdSpawn, Inbound, cargo_bin_exe_var,
    resolve_daemon_binary, wait_for_exit,
};

/// The env var `horizon-agentd`'s `main` reads to hold its
/// event-log-read-plus-resume phase until a file is there -- see that
/// binary's own doc comment on the constant of the same name. Test-only;
/// never set outside this file.
const TEST_RESUME_GATE_VAR: &str = "HORIZON_AGENT_TEST_RESUME_GATE";

/// The env var `horizon-agentd`'s `main` reads to have the mock provider's
/// slow turns wait, once their first word is out, until a file is there.
/// Test-only; never set outside this file.
const TEST_MOCK_STREAM_GATE_VAR: &str = "HORIZON_AGENT_TEST_MOCK_STREAM_GATE";

/// How long an answer that must come may take: only tells "never" apart.
const NEVER: Duration = Duration::from_secs(30);

/// A file a held phase waits for, not there yet; [`open_gate`] makes it.
fn gate_path(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hzn-gate-{what}-{}", uuid::Uuid::new_v4().simple()))
}

/// Lets what waits on `gate` go on.
fn open_gate(gate: &Path) {
    std::fs::write(gate, "").expect("make the gate");
}

/// Resolves the `horizon-agentd` binary to spawn. Only the `env!()` bake
/// has to be produced here (that macro expands only inside the package that
/// owns the `[[bin]]` target); the resolution rule itself, and the write-up
/// of why the runtime env var is preferred over that bake, live in
/// `horizon_daemon_testkit::binary`.
fn resolve_agentd_binary() -> PathBuf {
    resolve_daemon_binary("horizon-agent-acp", env!("CARGO_BIN_EXE_horizon-agent-acp"))
}

/// Proves the runtime resolution [`resolve_agentd_binary`] prefers
/// actually finds an existing binary, and that it's the same binary the
/// compile-time `env!()` bake would have named -- the mechanism backlog
/// #40's fix relies on: both are cargo's own idea of "the `horizon-agentd`
/// binary for this test run", differing only in *when* the value is
/// computed (build time vs. this exact invocation), not *what* it points
/// at, for a normal (non-stale-cache) run like this one.
#[test]
fn resolve_agentd_binary_finds_an_existing_binary_via_the_runtime_env_var() {
    let var = cargo_bin_exe_var("horizon-agent-acp");
    let runtime_var = std::env::var(&var)
        .expect("cargo/cargo-nextest must set CARGO_BIN_EXE_horizon-agent-acp at test runtime");
    let runtime_path = PathBuf::from(&runtime_var);
    assert!(
        runtime_path.is_file(),
        "runtime env var {var} = {runtime_var} does not point at an existing file"
    );

    let resolved = resolve_agentd_binary();
    assert_eq!(
        resolved, runtime_path,
        "resolve_agentd_binary must prefer the runtime env var over the compile-time bake"
    );

    // Deliberately NOT asserted: `resolved == env!("CARGO_BIN_EXE_...")`.
    // Divergence between the runtime var and the compile-time bake is this
    // fix's NORMAL operating mode under the shared build-dir -- it happens
    // whenever the cached test binary was compiled in a sibling worktree
    // (possibly since deleted). An equality assertion here failed the
    // integration gate the very first time that scenario occurred; see
    // `docs/tasks/backlog.md` #40.
}

/// The recipe every spawn below starts from: the testkit's hermetic
/// contract (throwaway event log and DuckDB projection, a deliberately
/// nonexistent config file, neutralized git config) plus this suite's own
/// test hooks explicitly cleared, so a hook set for one spawn can never
/// leak into the next.
fn agentd_spawn(paths: AgentdPaths) -> AgentdSpawn {
    AgentdSpawn::new(resolve_agentd_binary(), paths)
        .env_remove(TEST_RESUME_GATE_VAR)
        .env_remove(TEST_MOCK_STREAM_GATE_VAR)
}

/// Spawns `horizon-agentd` at fresh throwaway paths -- what almost every
/// test below wants.
fn spawn_agentd() -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch("agentd-e2e")).spawn()
}

/// Same as [`spawn_agentd`], but pointed at caller-chosen paths -- the seam
/// step 4's "kill -9 mid-session, respawn" tests use to bring a *second*
/// process up against the *first* process's own socket/event-log paths,
/// simulating a real restart.
fn spawn_agentd_at(sessions_dir: PathBuf) -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch_at("agentd-e2e", sessions_dir)).spawn()
}

/// Same as [`spawn_agentd_at`], but with `horizon-agentd`'s
/// event-log-read-plus-resume phase held until `gate` is made
/// ([`TEST_RESUME_GATE_VAR`]) -- for the bind-first ordering test:
/// `initialize` answering while it is held (and `session_list` not) is
/// then a consequence of the ordering, not of timing.
fn spawn_agentd_with_resume_gate(sessions_dir: PathBuf, gate: &Path) -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch_at("agentd-e2e", sessions_dir))
        .env(TEST_RESUME_GATE_VAR, gate)
        .spawn()
}

// --- the ACP test harness --------------------------------------------------

/// Connects to the real socket and completes `initialize` at this build's
/// extension version -- every session-hosting test's entry point.
async fn connect(agentd: &mut AgentdProcess) -> AcpClient {
    agentd.connect().await
}

fn test_cwd() -> PathBuf {
    std::env::current_dir().expect("test cwd should be readable")
}

fn acp_id(session_id: SessionId) -> v2::SessionId {
    v2::SessionId::new(session_id.as_uuid().to_string())
}

fn mock_provider_id() -> ProviderId {
    ProviderId("builtin.agent.mock".to_string())
}

/// A `session/new` on `provider_id`, confined to `cwd`.
fn new_session_request(provider_id: &str, cwd: PathBuf) -> v2::NewSessionRequest {
    v2::NewSessionRequest::new(cwd).config_options(initial_options(provider_id))
}

/// The initial `model` option value, on `provider_id`.
fn initial_options(provider_id: &str) -> Vec<v2::InitialSessionConfigOption> {
    vec![v2::InitialSessionConfigOption::new(
        acp::MODEL_CONFIG_ID,
        v2::SessionConfigOptionValue::id(acp::encode_model_option_id(provider_id, "default")),
    )]
}

/// A mock-provider session in the test's cwd.
fn session_new() -> v2::NewSessionRequest {
    new_session_request(&mock_provider_id().0, test_cwd())
}

/// Opens a session: the id the agent named it with.
async fn open_session(client: &AcpClient, request: v2::NewSessionRequest) -> SessionId {
    let response = client
        .request(request)
        .await
        .expect("session/new should succeed");
    SessionId::from_uuid(response.session_id.0.parse().expect("a uuid session id"))
}

async fn prompt(client: &AcpClient, session_id: SessionId, text: &str) {
    client
        .request(v2::PromptRequest::new(
            acp_id(session_id),
            vec![text.to_string().into()],
        ))
        .await
        .expect("session/prompt should be accepted");
}

async fn list_sessions(client: &AcpClient) -> Vec<v2::SessionInfo> {
    client
        .request(v2::ListSessionsRequest::new())
        .await
        .expect("session/list should succeed")
        .sessions
}

/// The one listed session, checked against its expected id and, when
/// given, its directory.
fn assert_listed(sessions: &[v2::SessionInfo], session_id: SessionId, cwd: Option<PathBuf>) {
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].session_id, acp_id(session_id));
    if let Some(cwd) = cwd {
        assert_eq!(sessions[0].cwd.0, cwd);
    }
}

/// The directory `session/list` gives `session_id`.
async fn listed_root(client: &AcpClient, session_id: SessionId) -> PathBuf {
    (list_sessions(client).await.into_iter())
        .find(|info| info.session_id == acp_id(session_id))
        .expect("the session is listed")
        .cwd
        .0
}

/// Waits for `session_id` to work outside `from`: each `session_info_update`
/// sends the client to `session/list` for its directory.
async fn moved_root(
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
async fn collect_until(
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
async fn resume(client: &mut AcpClient, session_id: SessionId) -> Result<Vec<Inbound>, Error> {
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

fn update(inbound: &Inbound) -> Option<&v2::SessionUpdate> {
    match inbound {
        Inbound::Update(notification) => Some(&notification.update),
        _ => None,
    }
}

fn content_text(content: &MaybeUndefined<Vec<v2::ContentBlock>>) -> String {
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

fn agent_message(inbound: &Inbound) -> Option<String> {
    match update(inbound)? {
        v2::SessionUpdate::AgentMessage(message) => Some(content_text(&message.content)),
        _ => None,
    }
}

fn user_message(inbound: &Inbound) -> Option<String> {
    match update(inbound)? {
        v2::SessionUpdate::UserMessage(message) => Some(content_text(&message.content)),
        _ => None,
    }
}

fn tool_update(inbound: &Inbound) -> Option<&v2::ToolCallUpdate> {
    match update(inbound)? {
        v2::SessionUpdate::ToolCallUpdate(call) => Some(call),
        _ => None,
    }
}

fn has_status(call: &v2::ToolCallUpdate, status: v2::ToolCallStatus) -> bool {
    call.status == MaybeUndefined::Value(status)
}

/// A tool call's final update: the one whose status ends it.
fn finished_tool(inbound: &Inbound) -> Option<&v2::ToolCallUpdate> {
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
fn tool_name(collected: &[Inbound], id: &v2::ToolCallId) -> Option<String> {
    (collected.iter().filter_map(tool_update))
        .filter(|call| call.tool_call_id == *id)
        .find_map(|call| call.name.as_opt_ref().flatten().cloned())
}

fn idle_stop_reason(inbound: &Inbound) -> Option<Option<v2::StopReason>> {
    match update(inbound)? {
        v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle)) => {
            Some(idle.stop_reason.clone())
        }
        _ => None,
    }
}

fn is_state(inbound: &Inbound) -> bool {
    matches!(update(inbound), Some(v2::SessionUpdate::StateUpdate(_)))
}

/// An error the agent reports: an error notice, or the error a failed
/// turn's stop reason carries.
fn session_error(inbound: &Inbound) -> Option<&str> {
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
struct Approval {
    occurrence_id: String,
}

fn approval_meta(request: &v2::RequestPermissionRequest) -> Approval {
    let Some(v2::RequestPermissionSubject::ToolCall(subject)) = &request.subject else {
        panic!("the permission names its tool call: {request:?}");
    };
    Approval {
        occurrence_id: subject.tool_call.tool_call_id.0.to_string(),
    }
}

fn take_permission(
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

fn approve() -> v2::RequestPermissionResponse {
    v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Selected(
        v2::SelectedPermissionOutcome::new(acp::PERMISSION_OPTION_APPROVE),
    ))
}

/// Everything comparable a message carries, requests' responders aside.
fn wire(inbound: &Inbound) -> serde_json::Value {
    match inbound {
        Inbound::Update(notification) => serde_json::to_value(notification),
        Inbound::Permission(request, _) => serde_json::to_value(request),
        Inbound::Replied(_) => Ok(serde_json::Value::Null),
    }
    .unwrap()
}

fn run_fixture_git(dir: &Path, args: &[&str]) {
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

fn isolated_session_fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("create isolated-session fixture repo");
    fixture_repository(repo.path());
    repo
}

/// A session's repository one level inside a directory of the test's own:
/// what a call writes beside the repository is then the test's to throw
/// away. The directory, and the repository in it.
fn nested_session_fixture() -> (tempfile::TempDir, PathBuf) {
    let outer = tempfile::tempdir().expect("create nested-session fixture");
    let repo = outer.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    fixture_repository(&repo);
    (outer, repo)
}

/// Makes `repo` a repository with one commit.
fn fixture_repository(repo: &Path) {
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
fn write_session_fixture(path: &std::path::Path, sessions: Vec<(SessionId, Vec<Event>)>) {
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
async fn wait_for_persisted_event(
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

// --- tests -----------------------------------------------------------------

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
    let mut agentd = agentd_spawn(AgentdPaths::scratch("agentd-switch"))
        .env("HORIZON_CONFIG", &config_path)
        .env_remove("HORIZON_SWITCH_TEST_KEY")
        .spawn();
    let mut client = connect(&mut agentd).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sessions_effort_starts_from_the_config_and_switches_over_acp() {
    /// The current value and the entries of the `effort` option an
    /// inbound config-option update carries.
    fn effort(inbound: &Inbound) -> Option<(String, usize)> {
        let v2::SessionUpdate::ConfigOptionUpdate(update) = update(inbound)? else {
            return None;
        };
        update.config_options.iter().find_map(|option| {
            let v2::SessionConfigKind::Select(select) = &option.kind else {
                return None;
            };
            let v2::SessionConfigSelectOptions::Ungrouped(entries) = &select.options else {
                return None;
            };
            (&*option.config_id.0 == acp::EFFORT_CONFIG_ID)
                .then(|| (select.current_value.0.to_string(), entries.len()))
        })
    }
    async fn announced(client: &mut AcpClient, expected: &str) -> usize {
        let collected = collect_until(client, |inbound| {
            effort(inbound).is_some_and(|(current, _)| current == expected)
        })
        .await;
        collected.iter().rev().find_map(effort).unwrap().1
    }
    fn set_effort(session_id: SessionId, value: &str) -> v2::SetSessionConfigOptionRequest {
        v2::SetSessionConfigOptionRequest::new(
            acp_id(session_id),
            acp::EFFORT_CONFIG_ID,
            v2::SessionConfigOptionValue::id(value.to_string()),
        )
    }

    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"default_model = { provider = "gateway", model = "gateway-model", effort = "high" }
[[providers]]
name = "gateway"
base_url = "http://127.0.0.1:1/v1"
api_key_env = "HORIZON_EFFORT_TEST_KEY"
"#,
    )
    .unwrap();
    let mut agentd = agentd_spawn(AgentdPaths::scratch("agentd-effort"))
        .env("HORIZON_CONFIG", &config_path)
        .env_remove("HORIZON_EFFORT_TEST_KEY")
        .spawn();
    let mut client = connect(&mut agentd).await;
    let session_id = open_session(
        &client,
        new_session_request(
            &horizon_agent::registry::named_rig_provider_id("gateway").0,
            test_cwd(),
        ),
    )
    .await;
    // A model no catalog lists takes every level.
    assert_eq!(announced(&mut client, "high").await, 7);

    client.request(set_effort(session_id, "low")).await.unwrap();
    announced(&mut client, "low").await;
    assert!(
        client
            .request(set_effort(session_id, "bogus"))
            .await
            .is_err()
    );

    let replay = resume(&mut client, session_id).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|inbound| effort(inbound).is_some_and(|(current, _)| current == "low")),
        "a reattachment announces the applied effort: {replay:?}"
    );

    // The choice is kept on the session's branch: after a restart the
    // session runs at it again.
    drop(client);
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(status.success(), "the drained daemon ended with {status:?}");
    let mut agentd = agentd.respawn_at_same_paths();
    let mut client = connect(&mut agentd).await;
    let replay = resume(&mut client, session_id).await.unwrap();
    if !replay
        .iter()
        .any(|inbound| effort(inbound).is_some_and(|(current, _)| current == "low"))
    {
        announced(&mut client, "low").await;
    }
}

/// `session/new` -> `session/prompt` -> the resulting updates arrive in
/// the order the mock provider produced them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_agent_then_user_message_streams_events_in_order() {
    let mut agentd = spawn_agentd();
    let mut client = connect(&mut agentd).await;

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
    let mut agentd = spawn_agentd();
    let mut client = connect(&mut agentd).await;
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
    let mut agentd = spawn_agentd();
    let client = connect(&mut agentd).await;

    let session_id = open_session(&client, session_new()).await;

    let sessions = list_sessions(&client).await;
    assert_listed(&sessions, session_id, Some(test_cwd()));
    assert_eq!(sessions[0].cwd, v2::AbsolutePath::new(test_cwd()));
}

/// Answers one control-plane connection at `socket`: the handshake, then
/// `snapshot` as the workspace. Returns what was asked.
fn serve_workspace_once(
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

/// `workspace_snapshot` runs in the agent, which asks the control plane of
/// the shell that started it (`HORIZON_SOCKET`) for the workspace, and the
/// answer is the finished tool call's output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_snapshot_asks_the_shells_control_plane() {
    let socket = horizon_daemon_testkit::scratch_socket("agent-e2e-control");
    let control = serve_workspace_once(&socket, serde_json::json!({ "tab_count": 1 }));
    let mut agentd = agentd_spawn(AgentdPaths::scratch("agentd-e2e"))
        .env("HORIZON_SOCKET", &socket)
        .spawn();
    let mut client = connect(&mut agentd).await;

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
            call.title == MaybeUndefined::Value("workspace_snapshot".to_string())
                && has_status(call, v2::ToolCallStatus::Pending)
        }),
        "expected the tool call to have been requested too, got: {collected:?}"
    );
}

/// Approval round trip: the approval goes out as
/// `session/request_permission`, the approving answer comes back, and
/// agentd runs the call and reports it as ordinary tool-call updates.
async fn approve_and_finish(
    client: &mut AcpClient,
    text: &str,
) -> (SessionId, Approval, Vec<Inbound>) {
    approve_and_finish_in(client, session_new(), text).await
}

/// [`approve_and_finish`] for a session opened with `request`.
async fn approve_and_finish_in(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_round_trip_request_out_approve_in_result_event_out() {
    let mut agentd = spawn_agentd();
    let mut client = connect(&mut agentd).await;
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

/// A session started in a plain checkout, the shape whose `bash` calls
/// run sandboxed without approval and ask to run outside it.
fn shared_session_in(cwd: PathBuf) -> v2::NewSessionRequest {
    new_session_request(&mock_provider_id().0, cwd)
}

/// `bash` runs agentd-side: approving a real `bash` tool call (a write
/// beside the workspace, asked for outside the sandbox) spawns an actual
/// subprocess in agentd, and the result arrives back as the call's final
/// update.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_runs_agentd_side_and_reports_its_result_over_the_wire() {
    let mut agentd = spawn_agentd();
    let mut client = connect(&mut agentd).await;
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
    assert_eq!(output["output"], "agentd-bash-ok\n");
}

/// Regression test for the 2026-07 repeated-approval OOM incident: a call
/// approved again and again -- each reattachment re-asks a still-pending
/// approval, and every ask is answered -- must start exactly once, both in
/// the updates and the persisted log, because a session's commands are
/// processed one at a time on its own dedicated thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_rapid_approve_of_the_same_call_starts_bash_exactly_once() {
    let mut agentd = spawn_agentd();
    let mut client = connect(&mut agentd).await;

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
        replay = resume(&mut client, session_id).await.unwrap();
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
        let candidate = horizon_agent::persistence::event_log::read_all(&agentd.sessions_dir)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_event_log_lines_are_reported_in_the_agents_log() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-sessions-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    // A live session whose log ends in a corrupt line and a torn tail.
    let damaged = SessionId::new();
    write_session_fixture(
        &sessions_dir,
        vec![(
            damaged,
            vec![Event::StateChanged(SessionState::WaitingForUser)],
        )],
    );
    {
        use std::io::Write;
        let path = horizon_agent::persistence::event_log::session_path(&sessions_dir, damaged);
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"not valid json\n{\"text\":\"\xe6\x97")
            .expect("write corrupt line and a tail split inside UTF-8");
    }

    let mut agentd = agentd_spawn(AgentdPaths::scratch_at("agentd-e2e", sessions_dir))
        .capture_stderr()
        .spawn();
    let _client = connect(&mut agentd).await;
    agentd
        .wait_for_stderr_line(
            "skipped 1 corrupt line and a torn trailing line while reading the session logs",
        )
        .await;

    // The ignored tail must not join the first record the resumed session
    // writes.
    wait_for_persisted_event(&agentd.sessions_dir, damaged, |event| {
        matches!(event, Event::StateChanged(SessionState::Created))
    })
    .await;
    let report =
        horizon_agent::persistence::event_log::read_session(&agentd.sessions_dir, damaged).unwrap();
    assert_eq!(
        report.corrupt_line_count, 1,
        "only the original corrupt line remains"
    );
    assert!(!report.ignored_partial_line);
    assert_eq!(report.records.first().unwrap().sequence, 0);
}

/// Step 4's headline scenario: `kill -9` a live daemon mid-session (a turn
/// genuinely still open, waiting for approval), respawn against the same
/// log, and confirm replay: transcript survives, the interrupted turn is
/// committed as cancelled, the session is immediately usable again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_agentd_respawns_and_replays_transcript_with_open_turn_cancelled() {
    let mut agentd = spawn_agentd();
    let sessions_dir = agentd.sessions_dir.clone();
    let mut client = connect(&mut agentd).await;

    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "please run a tool").await;
    collect_until(&mut client, |inbound| {
        matches!(inbound, Inbound::Permission(..))
    })
    .await;
    wait_for_persisted_event(&sessions_dir, session_id, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;

    let interrupted_turn_id = horizon_agent::persistence::event_log::read_all(&sessions_dir)
        .unwrap()
        .records
        .into_iter()
        .find(|record| {
            record.session_id == session_id && matches!(record.event, Event::ApprovalRequested(_))
        })
        .unwrap()
        .turn_id
        .expect("approval belongs to the original turn");

    drop(client);
    agentd.kill_and_wait();

    // Fresh projection path, so this start rebuilds; stderr is captured to
    // wait for that rebuild below.
    let mut respawned = agentd_spawn(AgentdPaths::scratch_at("agentd-e2e", sessions_dir))
        .capture_stderr()
        .spawn();
    let mut client = connect(&mut respawned).await;

    assert_listed(&list_sessions(&client).await, session_id, Some(test_cwd()));

    let replayed = resume(&mut client, session_id).await.unwrap();
    assert!(
        replayed
            .iter()
            .any(|inbound| user_message(inbound).as_deref() == Some("please run a tool")),
        "the pre-crash user message must survive replay, got: {replayed:?}"
    );
    assert!(
        replayed
            .iter()
            .any(|inbound| { idle_stop_reason(inbound) == Some(Some(v2::StopReason::Cancelled)) }),
        "the interrupted turn must be committed as cancelled on resume, got: {replayed:?}"
    );
    for requested in replayed
        .iter()
        .filter_map(tool_update)
        .filter(|call| has_status(call, v2::ToolCallStatus::Pending))
    {
        assert!(
            replayed
                .iter()
                .filter_map(finished_tool)
                .any(|call| call.tool_call_id == requested.tool_call_id),
            "resume must close the exact original execution"
        );
    }
    assert!(
        idle_stop_reason(
            replayed
                .iter()
                .rev()
                .find(|inbound| is_state(inbound))
                .unwrap()
        )
        .is_some(),
        "replay must leave the session ready for a new turn, got: {replayed:?}"
    );
    assert!(
        !replayed
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "the cancelled approval must not be asked again"
    );

    prompt(&client, session_id, "hello again").await;
    let collected = collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: hello again")
    })
    .await;
    assert!(
        !collected
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "the cancelled approval must not be asked again"
    );
    assert!(
        collected
            .iter()
            .any(|inbound| user_message(inbound).as_deref() == Some("hello again"))
    );

    wait_for_persisted_event(&respawned.sessions_dir, session_id, |event| {
        matches!(event, Event::MessageCommitted(message)
            if message.role == MessageRole::Assistant && message.text == "Mock response: hello again")
    })
    .await;
    drop(client);
    assert!(wait_for_exit(&mut respawned.child).await.success());

    let records = horizon_agent::persistence::event_log::read_all(&respawned.sessions_dir)
        .unwrap()
        .records;
    let cancelled = records
        .iter()
        .find(|record| {
            record.session_id == session_id
                && matches!(record.event, Event::TurnEnded(TurnEndReason::Cancelled))
        })
        .unwrap();
    assert_eq!(cancelled.turn_id.as_ref(), Some(&interrupted_turn_id));
    let next_turn = records
        .iter()
        .find(|record| {
            record.session_id == session_id
                && matches!(&record.event, Event::MessageCommitted(message)
                    if message.role == MessageRole::User && message.text == "hello again")
        })
        .unwrap();
    assert!(next_turn.turn_id.is_some());
    assert_ne!(next_turn.turn_id, cancelled.turn_id);
    horizon_agent::persistence::projection::duckdb::Projection::new(
        &respawned.sessions_dir,
        &respawned.state_db_path,
    )
    .catch_up()
    .expect("catch the projection up");
    let conn = duckdb::Connection::open(&respawned.state_db_path).unwrap();
    let projected: (String, String) = conn.query_row(
        "SELECT turn_id, ended_event_id FROM agent_turns WHERE session_id = ? AND end_reason = 'cancelled'",
        [session_id.as_uuid().to_string()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).expect("the recovered turn must reach the real DuckDB projection");
    assert_eq!(projected, (interrupted_turn_id, cancelled.event_id.clone()));
    let next_projected: String = conn
        .query_row(
            "SELECT turn_id FROM agent_events WHERE event_id = ?",
            [&next_turn.event_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(Some(next_projected), next_turn.turn_id);
}

/// A daemon restart must preserve the authoritative root and tier-1
/// eligibility of a session that entered a worktree. The regression this
/// pins down downgraded every resumed session to the daemon cwd with
/// `isolated=false`, so later bash calls escaped the per-command sandbox and
/// requested manual approval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_re_adopts_an_isolated_worktree_and_keeps_bash_contained() {
    let repo = isolated_session_fixture();
    let mut agentd = spawn_agentd();
    let sessions_dir = agentd.sessions_dir.clone();
    let mut client = connect(&mut agentd).await;
    let session_id = open_session(&client, shared_session_in(repo.path().to_path_buf())).await;

    prompt(&client, session_id, "please enter a worktree").await;
    let (_, isolated_root) = moved_root(&mut client, session_id, repo.path()).await;
    assert!(
        isolated_root.starts_with(
            repo.path()
                .canonicalize()
                .unwrap()
                .join(".horizon/worktrees")
        ),
        "resolved root should be the session-owned worktree: {}",
        isolated_root.display()
    );

    wait_for_persisted_event(
        &sessions_dir,
        session_id,
        |event| matches!(event, Event::EnvironmentActivated(identity) if identity.path == isolated_root),
    )
    .await;
    let report = horizon_agent::persistence::event_log::read_all(&sessions_dir)
        .expect("the session context should be readable before restart");
    assert!(report.records.iter().any(|record| {
        record.session_id == session_id
            && record.session_context.as_ref().is_some_and(|context| {
                context.workspace_root.as_ref() == Some(&isolated_root) && context.isolated_worktree
            })
    }));

    drop(client);
    agentd.kill_and_wait();

    let mut respawned = spawn_agentd_at(sessions_dir);
    let mut client = connect(&mut respawned).await;
    let resumed = list_sessions(&client)
        .await
        .into_iter()
        .find(|info| info.session_id == acp_id(session_id))
        .expect("the isolated session should resume live");
    assert_eq!(Some(&resumed.cwd.0), Some(&isolated_root));

    let _ = resume(&mut client, session_id).await.unwrap();
    if !horizon_agent::tools::sandbox::ready() {
        return;
    }

    prompt(&client, session_id, "please run bash").await;
    let collected = collect_until(&mut client, |inbound| finished_tool(inbound).is_some()).await;
    assert!(
        !collected
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "a resumed isolated session should retain tier-1 auto execution: {collected:?}"
    );
    let call = collected.iter().rev().find_map(finished_tool).unwrap();
    let output = call.raw_output.as_opt_ref().flatten().unwrap();
    assert_eq!(
        output["auto_approved"], true,
        "resumed call should retain the contained classification: {output:?}"
    );
    assert_eq!(
        output["policy_tier"], "contained",
        "resumed call should retain tier-1 eligibility: {output:?}"
    );
}

/// Whether `inbound` is a `session_info_update` of `session_id`.
fn info_update(inbound: &Inbound, session_id: SessionId) -> bool {
    let Inbound::Update(notification) = inbound else {
        return false;
    };
    notification.session_id == acp_id(session_id)
        && matches!(notification.update, v2::SessionUpdate::SessionInfoUpdate(_))
}

fn fixture_git_output(dir: &Path, args: &[&str]) -> String {
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

/// `worktree_enter` end to end: a shared session asks for a worktree, the
/// activation lands at the turn boundary, the client learns the new root
/// live, later writes are confined to it, and a daemon restart resumes the
/// session in the same worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_enter_moves_a_shared_session_and_survives_a_restart() {
    let repo = isolated_session_fixture();
    let head = fixture_git_output(repo.path(), &["rev-parse", "HEAD"]);
    let mut agentd = spawn_agentd();
    let sessions_dir = agentd.sessions_dir.clone();
    let mut client = connect(&mut agentd).await;
    let session_id = open_session(&client, shared_session_in(repo.path().to_path_buf())).await;

    prompt(&client, session_id, "please enter a worktree").await;
    let (collected, root) = moved_root(&mut client, session_id, repo.path()).await;
    let call = collected
        .iter()
        .find_map(finished_tool)
        .expect("the worktree_enter call finishes before the switch");
    assert_eq!(
        tool_name(&collected, &call.tool_call_id).as_deref(),
        Some("worktree_enter")
    );
    let output = call.raw_output.as_opt_ref().flatten().unwrap();
    assert_eq!(output["requested"], true, "{output}");
    assert_eq!(output["base"], head.as_str(), "{output}");
    assert!(
        !collected
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "worktree_enter is automatic: {collected:?}"
    );

    assert_eq!(
        root.parent(),
        Some(
            repo.path()
                .canonicalize()
                .unwrap()
                .join(".horizon/worktrees")
                .as_path()
        )
    );
    assert_eq!(fixture_git_output(&root, &["rev-parse", "HEAD"]), head);
    let branch = fixture_git_output(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(branch.starts_with("horizon/"), "{branch}");
    let listed = list_sessions(&client)
        .await
        .into_iter()
        .find(|info| info.session_id == acp_id(session_id))
        .unwrap();
    assert_eq!(Some(&listed.cwd.0), Some(&root));

    if horizon_agent::tools::sandbox::ready() {
        prompt(&client, session_id, "please write the marker").await;
        let collected =
            collect_until(&mut client, |inbound| finished_tool(inbound).is_some()).await;
        assert!(
            !collected
                .iter()
                .any(|inbound| matches!(inbound, Inbound::Permission(..))),
            "bash in the entered worktree runs sandboxed without approval: {collected:?}"
        );
        let call = collected.iter().rev().find_map(finished_tool).unwrap();
        let output = call.raw_output.as_opt_ref().flatten().unwrap();
        assert_eq!(output["policy_tier"], "contained", "{output}");
        assert_eq!(output["exit_code"], 0, "{output}");
        assert_eq!(
            output["output"].as_str().unwrap().trim(),
            root.display().to_string(),
            "bash starts in the new root: {output}"
        );
        assert!(root.join("worktree-marker").exists());
        assert!(!repo.path().join("worktree-marker").exists());
    }

    let inside = root.join("inside.txt");
    prompt(
        &client,
        session_id,
        &format!("write file {}", inside.display()),
    )
    .await;
    let collected = collect_until(&mut client, |inbound| finished_tool(inbound).is_some()).await;
    assert!(
        !collected
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "fs_write inside the new root is automatic: {collected:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&inside).unwrap(),
        "written by the mock\n"
    );

    wait_for_persisted_event(
        &sessions_dir,
        session_id,
        |event| matches!(event, Event::EnvironmentActivated(identity) if identity.path == root),
    )
    .await;
    drop(client);
    agentd.kill_and_wait();

    let mut respawned = spawn_agentd_at(sessions_dir);
    let mut client = connect(&mut respawned).await;
    let resumed = list_sessions(&client)
        .await
        .into_iter()
        .find(|info| info.session_id == acp_id(session_id))
        .expect("the session resumes live");
    assert_eq!(Some(&resumed.cwd.0), Some(&root));
    let _ = resume(&mut client, session_id).await.unwrap();

    // The old root is now outside the session's root: `fs_write` refuses it.
    let outside = repo.path().join("outside.txt");
    prompt(
        &client,
        session_id,
        &format!("write file {}", outside.display()),
    )
    .await;
    let collected = collect_until(&mut client, |inbound| finished_tool(inbound).is_some()).await;
    let call = collected.iter().rev().find_map(finished_tool).unwrap();
    let output = call.raw_output.as_opt_ref().flatten().unwrap();
    assert_eq!(output["is_error"], true, "{output}");
    assert!(!outside.exists());
}

/// The server-side substance of `Reload Agent Runtime`: drain a live
/// session gracefully (not a crash), respawn against the same paths, and
/// confirm the session survives with its transcript intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drained_agentd_respawns_and_preserves_a_completed_session() {
    let mut agentd = spawn_agentd();
    let sessions_dir = agentd.sessions_dir.clone();
    let mut client = connect(&mut agentd).await;

    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "hello").await;
    collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: hello")
    })
    .await;

    drop(client);
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(status.success(), "drain should exit 0, got {status:?}");

    let mut respawned = spawn_agentd_at(sessions_dir);
    let mut client = connect(&mut respawned).await;
    assert_listed(&list_sessions(&client).await, session_id, Some(test_cwd()));

    let replayed = resume(&mut client, session_id).await.unwrap();
    assert!(
        replayed
            .iter()
            .any(|inbound| user_message(inbound).as_deref() == Some("hello")),
        "the pre-drain transcript must survive, got: {replayed:?}"
    );
    assert!(
        !replayed
            .iter()
            .any(|inbound| { idle_stop_reason(inbound) == Some(Some(v2::StopReason::Cancelled)) }),
        "a turn that had already completed cleanly before the drain must not be \
         re-marked as cancelled on resume, got: {replayed:?}"
    );
}

/// Fix 2: a session whose log already ends in a terminal state must not be
/// resumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_skips_sessions_whose_log_already_ended_in_a_terminal_state() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-events-{}-{}.jsonl",
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

    let mut agentd = spawn_agentd_at(sessions_dir);
    let client = connect(&mut agentd).await;
    assert_listed(&list_sessions(&client).await, live_session, None);
}

/// Fix 1: `initialize` must answer well before a slow resume finishes, and
/// `session/list` must wait for it -- proven with the resume-delay hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_answers_immediately_while_session_list_waits_for_a_slow_resume() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-events-{}-{}.jsonl",
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
    let mut agentd = spawn_agentd_with_resume_gate(sessions_dir, &gate);

    // `initialize` answers while the resume is held...
    let client = tokio::time::timeout(NEVER, connect(&mut agentd))
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

/// A crash between durable announcement and dispatch still leaves a valid
/// conversation after restoration, including another full daemon restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rig_conversation_survives_an_undispatched_call_and_repeated_daemon_restart() {
    use horizon_agent::contract::{
        ConversationInputKind, ConversationRecord, OccurrenceId, ToolCallId, ToolCallIdentity,
        ToolOutcome,
    };
    let paths = AgentdPaths::scratch("rig-history-restart");
    let session = SessionId::new();
    let identity = ToolCallIdentity {
        call_id: ToolCallId("pending".into()),
        occurrence_id: OccurrenceId::new(),
    };
    let writer = WriterHandle::open(&paths.sessions_dir);
    let mut appender = Appender::new(
        writer.clone(),
        session,
        Some(ProviderId("builtin.agent.rig".into())),
        None,
    );
    appender.commit_provider_events(vec![
        Event::StateChanged(SessionState::Running),
        Event::ConversationRecorded(ConversationRecord::TurnOpened),
        Event::ConversationRecorded(ConversationRecord::Input {kind:ConversationInputKind::User,text:"earlier question".into()}),
        Event::ConversationRecorded(ConversationRecord::ToolAnnounced {
            response_id:"interrupted-response".into(),identity:identity.clone(),codec:2,
            tool_call:serde_json::json!({"id":{"provider":"pending"},"function":{"name":"fs_read","arguments":{"path":"never-dispatched"}}}).into(),
            reasoning:serde_json::json!([]).into(),
        }),
    ].into_iter().map(Into::into).collect()).unwrap();
    writer.flush().unwrap();
    drop(appender);
    drop(writer);
    let mut daemon = agentd_spawn(paths)
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .spawn();
    let log = daemon.sessions_dir.clone();
    for round in 0..2 {
        let mut client = connect(&mut daemon).await;
        let _ = resume(&mut client, session).await.unwrap();
        prompt(&client, session, &format!("hello after restart {round}")).await;
        let collected = collect_until(&mut client, |inbound| {
            idle_stop_reason(inbound) == Some(Some(v2::StopReason::EndTurn))
        })
        .await;
        assert!(
            !collected
                .iter()
                .any(|inbound| session_error(inbound).is_some()),
            "{collected:?}"
        );
        drop(client);
        let status = wait_for_exit(&mut daemon.child).await;
        assert!(status.success(), "the drained daemon ended with {status:?}");
        let records = horizon_agent::persistence::event_log::read_all(&log)
            .unwrap()
            .records;
        assert_eq!(records.iter().filter(|record|matches!(&record.event,Event::ToolCallFinished(result) if result.occurrence_id==identity.occurrence_id && result.outcome==ToolOutcome::Cancelled)).count(),1);
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    &record.event,
                    Event::ConversationRecorded(ConversationRecord::Input {
                        kind: ConversationInputKind::User,
                        ..
                    })
                ))
                .count(),
            round + 2
        );
        horizon_agent::persistence::validate_history(&log).unwrap();
        if round == 0 {
            daemon = daemon.respawn_at_same_paths();
        }
    }
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
    let mut agentd = agentd_spawn(AgentdPaths::scratch("agentd-e2e"))
        .env(TEST_MOCK_STREAM_GATE_VAR, &gate)
        .spawn();
    let mut client = connect(&mut agentd).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_history_survives_abandoned_replay_and_daemon_restart() {
    fn user_texts(messages: &[Inbound]) -> Vec<String> {
        messages
            .iter()
            .filter_map(user_message)
            .filter(|text| text.starts_with("historical message "))
            .collect()
    }
    let temp = tempfile::tempdir().unwrap();
    let session = SessionId::new();
    let log = temp.path().join("events.jsonl");
    let history: Vec<_> = (0..3000)
        .map(|index| format!("historical message {index}: {}", "x".repeat(512)))
        .collect();
    write_session_fixture(
        &log,
        vec![(
            session,
            history
                .iter()
                .map(|text| {
                    Event::MessageCommitted(horizon_agent::contract::Message {
                        role: MessageRole::User,
                        text: text.clone(),
                    })
                })
                .collect(),
        )],
    );
    let mut agentd = spawn_agentd_at(log.clone());
    let mut abandoned = connect(&mut agentd).await;
    abandoned.send_ordered(
        v2::ResumeSessionRequest::new(acp_id(session), test_cwd())
            .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
    );
    let _first = abandoned.next(Duration::from_secs(120)).await;
    // Closing the connection mid-replay ends the process.
    drop(abandoned);
    let mut agentd = agentd.respawn_at_same_paths();
    let mut client = connect(&mut agentd).await;
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&replay), history);
    drop(client);
    agentd.kill_and_wait();
    let mut restarted = spawn_agentd_at(log);
    let mut client = connect(&mut restarted).await;
    let restored = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&restored), history);
}

/// The user messages on `session`'s branch of the log, in order.
fn branch_user_messages(log: &Path, session: SessionId) -> Vec<String> {
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
async fn prompt_and_settle(
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
fn cursor_before_message(collected: &[Inbound], text: &str) -> v2::CursorId {
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

/// A session forked at the cursor before its second message holds the first
/// exchange and the files as they were then, in a worktree of its own; it
/// goes on by itself while the source keeps its own, and comes back with
/// that branch and worktree after the daemon restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_forked_at_a_cursor_goes_on_from_there_and_survives_a_restart() {
    let (_outer, repo) = nested_session_fixture();
    let mut daemon = agentd_spawn(AgentdPaths::scratch("agentd-e2e"))
        .env_remove("HORIZON_AGENT_TEST_NO_CHECKPOINTS")
        .spawn();
    let log = daemon.sessions_dir.clone();
    let mut client = connect(&mut daemon).await;
    let source = open_session(
        &client,
        new_session_request(&mock_provider_id().0, repo.clone()),
    )
    .await;
    prompt_and_settle(&mut client, source, "one").await;
    std::fs::write(repo.join("notes.txt"), "after one\n").unwrap();
    let collected = prompt_and_settle(&mut client, source, "two").await;
    std::fs::write(repo.join("notes.txt"), "after two\n").unwrap();
    let cursor = cursor_before_message(&collected, "two");

    let unknown = client
        .request(
            v2::ForkSessionRequest::new(acp_id(source), repo.clone())
                .cursor_id(v2::CursorId::new("before-nowhere")),
        )
        .await
        .expect_err("an unannounced cursor is refused");
    assert_eq!(
        unknown.code,
        agent_client_protocol::ErrorCode::INVALID_PARAMS
    );

    let forked = client
        .request(v2::ForkSessionRequest::new(acp_id(source), repo.clone()).cursor_id(cursor))
        .await
        .expect("session/fork at an announced cursor succeeds");
    let fork = SessionId::from_uuid(forked.session_id.0.parse().unwrap());
    assert_ne!(fork, source);
    // The fork works in a worktree of its own.
    let worktree = listed_root(&client, fork).await;
    assert!(
        worktree.starts_with(
            std::fs::canonicalize(&repo)
                .unwrap()
                .join(".horizon/worktrees")
        ),
        "{worktree:?}"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("notes.txt")).unwrap(),
        "after one\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "after two\n"
    );
    prompt_and_settle(&mut client, fork, "three").await;

    assert_eq!(branch_user_messages(&log, source), ["one", "two"]);
    assert_eq!(branch_user_messages(&log, fork), ["one", "three"]);
    horizon_agent::persistence::validate_history(&log).unwrap();

    drop(client);
    let status = wait_for_exit(&mut daemon.child).await;
    assert!(status.success(), "the drained daemon ended with {status:?}");
    daemon = daemon.respawn_at_same_paths();
    let mut client = connect(&mut daemon).await;
    let bootstrap = resume(&mut client, fork).await.unwrap();
    let replayed: Vec<_> = bootstrap.iter().filter_map(user_message).collect();
    assert_eq!(replayed, ["one", "three"]);
    let answered = prompt_and_settle(&mut client, fork, "four").await;
    assert!(
        answered
            .iter()
            .any(|inbound| agent_message(inbound).as_deref() == Some("Mock response: four")),
        "{answered:?}"
    );
    let listed = list_sessions(&client).await;
    let fork_info = listed
        .iter()
        .find(|info| info.session_id == acp_id(fork))
        .expect("the fork is listed");
    assert_eq!(Some(fork_info.cwd.0.clone()), Some(worktree));
    assert_eq!(branch_user_messages(&log, fork), ["one", "three", "four"]);
    assert_eq!(branch_user_messages(&log, source), ["one", "two"]);
}
