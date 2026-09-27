//! End-to-end test against the real `horizon-agentd` binary, spawned
//! through `horizon-daemon-testkit`: it owns binary resolution (the runtime
//! `CARGO_BIN_EXE_horizon-agentd` env var in preference to the same-named
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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::{v2, MaybeUndefined};
use agent_client_protocol::Error;
use horizon_acp as acp;
use horizon_agent::contract::{
    Event, Exit, MessageRole, ProviderEvent, ProviderId, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::persistence::event_log::{Appender, WriterHandle, WriterInit};
use horizon_daemon_testkit::{
    agentd_hermetic_command, cargo_bin_exe_var, connect_acp, connect_initialized,
    connect_with_retry, drain_uninitialized, initialize_request, resolve_daemon_binary,
    spawn_with_link_retry, wait_for_exit, AcpClient, AgentdPaths, AgentdProcess, AgentdSpawn,
    Inbound,
};

/// The env var `horizon-agentd`'s `main` reads to artificially delay its
/// event-log-read-plus-resume phase -- see that binary's own doc comment on
/// the constant of the same name. Test-only; never set outside this file.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_RESUME_DELAY_MS";

/// The env var `horizon-agentd`'s `main` reads to artificially delay its
/// background DuckDB rebuild task -- the DuckDB analogue of
/// [`TEST_RESUME_DELAY_MS_VAR`], letting a test prove `initialize`/
/// `session/list` stay reachable while a slow rebuild is still running.
/// Test-only; never set outside this file.
const TEST_DUCKDB_REBUILD_DELAY_MS_VAR: &str = "HORIZON_AGENTD_TEST_DUCKDB_REBUILD_DELAY_MS";

/// Resolves the `horizon-agentd` binary to spawn. Only the `env!()` bake
/// has to be produced here (that macro expands only inside the package that
/// owns the `[[bin]]` target); the resolution rule itself, and the write-up
/// of why the runtime env var is preferred over that bake, live in
/// `horizon_daemon_testkit::binary`.
fn resolve_agentd_binary() -> PathBuf {
    resolve_daemon_binary("horizon-agentd", env!("CARGO_BIN_EXE_horizon-agentd"))
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
    let var = cargo_bin_exe_var("horizon-agentd");
    let runtime_var = std::env::var(&var)
        .expect("cargo/cargo-nextest must set CARGO_BIN_EXE_horizon-agentd at test runtime");
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
        .env_remove(TEST_RESUME_DELAY_MS_VAR)
        .env_remove(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
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
fn spawn_agentd_at(socket_path: PathBuf, event_log_path: PathBuf) -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch_at(
        "agentd-e2e",
        socket_path,
        event_log_path,
    ))
    .spawn()
}

/// Same as [`spawn_agentd_at`], but additionally sets `horizon-agentd`'s
/// test-only [`TEST_RESUME_DELAY_MS_VAR`] hook -- for the bind-first
/// ordering test, which needs the log-read-plus-resume phase to take long
/// enough that hello answering before it finishes (and `session_list`
/// waiting for it) is provably a consequence of the ordering fix, not
/// incidental timing.
fn spawn_agentd_with_resume_delay(
    socket_path: PathBuf,
    event_log_path: PathBuf,
    resume_delay_ms: u64,
) -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch_at(
        "agentd-e2e",
        socket_path,
        event_log_path,
    ))
    .env(TEST_RESUME_DELAY_MS_VAR, resume_delay_ms.to_string())
    .spawn()
}

/// Same as [`spawn_agentd_at`], but additionally sets `horizon-agentd`'s
/// test-only [`TEST_DUCKDB_REBUILD_DELAY_MS_VAR`] hook -- for proving the
/// DuckDB rebuild (task 1 of the readiness fix) no longer sits on the
/// resume-readiness path `hello`/`session_list` block on.
fn spawn_agentd_with_duckdb_rebuild_delay(
    socket_path: PathBuf,
    event_log_path: PathBuf,
    rebuild_delay_ms: u64,
) -> AgentdProcess {
    agentd_spawn(AgentdPaths::scratch_at(
        "agentd-e2e",
        socket_path,
        event_log_path,
    ))
    .env(
        TEST_DUCKDB_REBUILD_DELAY_MS_VAR,
        rebuild_delay_ms.to_string(),
    )
    .spawn()
}

/// Same as [`spawn_agentd_at`], but with an explicit `state_db_path`
/// (rather than the fresh random one every other spawn picks) and piped,
/// continuously drained stderr (see `AgentdProcess::wait_for_stderr_line`)
/// -- both needed only by task 2's skip/rebuild tests below: they must
/// point two successive spawns at the *same* DuckDB file to prove the
/// second one either skips or redoes the rebuild, and must observe that
/// spawn's own rebuild-or-skip decision before killing the process.
fn spawn_agentd_with_duckdb_options(
    socket_path: PathBuf,
    event_log_path: PathBuf,
    state_db_path: PathBuf,
) -> AgentdProcess {
    let mut paths = AgentdPaths::scratch_at("agentd-e2e", socket_path, event_log_path);
    paths.state_db_path = state_db_path;
    agentd_spawn(paths).capture_stderr().spawn()
}

// --- the ACP test harness --------------------------------------------------

/// Connects to the real socket and completes `initialize` at this build's
/// extension version -- every session-hosting test's entry point.
async fn connect(socket_path: &Path) -> AcpClient {
    connect_initialized(connect_with_retry(socket_path).await, "test-client").await
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

/// A `session/new` for `session_id` on `provider_id`, confined to `cwd`.
fn new_session_request(
    session_id: SessionId,
    provider_id: &str,
    role_id: Option<&str>,
    cwd: PathBuf,
    isolate: bool,
) -> v2::NewSessionRequest {
    let mut meta = None;
    acp::write_horizon_meta(
        &mut meta,
        &acp::SessionNewMeta {
            session_id,
            provider_id: provider_id.to_string(),
            role_id: role_id.map(str::to_string),
            isolate,
            spawn_source_session_id: None,
        },
    )
    .unwrap();
    v2::NewSessionRequest::new(cwd).meta(meta)
}

/// A mock-provider session in the test's cwd.
fn session_new(session_id: SessionId) -> v2::NewSessionRequest {
    new_session_request(session_id, &mock_provider_id().0, None, test_cwd(), false)
}

async fn open_session(
    client: &AcpClient,
    request: v2::NewSessionRequest,
) -> v2::NewSessionResponse {
    client
        .request(request)
        .await
        .expect("session/new should succeed")
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

fn info_meta(info: &v2::SessionInfo) -> acp::SessionInfoMeta {
    acp::read_horizon_meta(info.meta.as_ref())
        .expect("session info carries _meta.horizon")
        .unwrap()
}

/// The one listed session, checked against its expected id and facts.
fn assert_listed(
    sessions: &[v2::SessionInfo],
    session_id: SessionId,
    role_id: Option<&str>,
    workspace_root: Option<PathBuf>,
) {
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].session_id, acp_id(session_id));
    assert_eq!(
        info_meta(&sessions[0]),
        acp::SessionInfoMeta {
            workspace_root,
            parent_session_id: None,
            role_id: role_id.map(str::to_string),
            provider_id: mock_provider_id().0,
        }
    );
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

fn tool_meta(call: &v2::ToolCallUpdate) -> Option<acp::ToolCallMeta> {
    acp::read_horizon_meta(call.meta.as_opt_ref().flatten())?.ok()
}

fn has_status(call: &v2::ToolCallUpdate, status: v2::ToolCallStatus) -> bool {
    call.status == MaybeUndefined::Value(status)
}

/// A tool call's final update: the one that carries its outcome.
fn finished_tool(inbound: &Inbound) -> Option<(&v2::ToolCallUpdate, acp::ToolCallMeta)> {
    let call = tool_update(inbound)?;
    let meta = tool_meta(call)?;
    meta.outcome.is_some().then_some((call, meta))
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

fn session_error(inbound: &Inbound) -> Option<&str> {
    match inbound {
        Inbound::SessionEvent(acp::SessionEventNotification::Error { message, .. }) => {
            Some(message)
        }
        _ => None,
    }
}

/// The permission request's approval payload.
fn approval_meta(request: &v2::RequestPermissionRequest) -> acp::ApprovalMeta {
    acp::read_horizon_meta(request.meta.as_ref())
        .expect("permission requests carry _meta.horizon")
        .unwrap()
}

fn take_permission(
    collected: &mut Vec<Inbound>,
) -> (
    v2::RequestPermissionRequest,
    agent_client_protocol::Responder<v2::RequestPermissionResponse>,
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
        Inbound::SessionEvent(notification) => serde_json::to_value(notification),
        Inbound::TaskProgress(notification) => serde_json::to_value(notification),
        Inbound::ToolCallProgress(notification) => serde_json::to_value(notification),
        Inbound::Memory(notification) => serde_json::to_value(notification),
        Inbound::ProviderRequest(notification) => serde_json::to_value(notification),
        Inbound::Permission(request, _) => serde_json::to_value(request),
        Inbound::HostTool(request, _) => serde_json::to_value(request),
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
    run_fixture_git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    run_fixture_git(repo.path(), &["add", "README.md"]);
    run_fixture_git(
        repo.path(),
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
    repo
}

/// Writes a fixture event log directly at `path`, one session per
/// `(SessionId, Vec<Event>)` pair, via the same `WriterHandle`/`Appender`
/// machinery `horizon-agent`'s own event-log tests use -- for tests below
/// that need a specific pre-existing log *before* `horizon-agentd` itself
/// ever runs. Every record gets [`mock_provider_id`] as its provider id.
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
        if let Ok(report) = horizon_agent::persistence::event_log::read(path) {
            if report
                .records
                .iter()
                .any(|record| record.session_id == session_id && predicate(&record.event))
            {
                return;
            }
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
    let initial = r#"default_provider = "initial"
[[providers]]
name = "initial"
api_key_env = "HORIZON_SWITCH_TEST_KEY"
default_model = "before-model"
"#;
    std::fs::write(&config_path, initial).unwrap();
    let agentd = agentd_spawn(AgentdPaths::scratch("agentd-switch"))
        .env("HORIZON_CONFIG", &config_path)
        .env_remove("HORIZON_SWITCH_TEST_KEY")
        .spawn();
    let mut client = connect(&agentd.socket_path).await;
    let session_id = SessionId::new();
    let created = open_session(
        &client,
        new_session_request(
            session_id,
            &horizon_agent::registry::named_rig_provider_id("initial").0,
            None,
            test_cwd(),
            false,
        ),
    )
    .await;
    assert_eq!(created.session_id, acp_id(session_id));
    // Ensure the provider thread has captured its initial config before reload.
    collect_until(&mut client, |inbound| idle_stop_reason(inbound).is_some()).await;

    std::fs::write(
        &config_path,
        format!(
            "{initial}\n[[providers]]\nname = \"added\"\nkind = \"anthropic\"\n\
             api_key_env = \"HORIZON_SWITCH_TEST_KEY\"\ndefault_model = \"after-model\"\n"
        ),
    )
    .unwrap();
    client
        .request(acp::ReloadProviderConfigRequest {})
        .await
        .unwrap();
    client
        .request(set_model(session_id, "added/after-model"))
        .await
        .unwrap();
    applied(&mut client, "added/after-model").await;

    client
        .request(set_model(session_id, "added/wire-model"))
        .await
        .unwrap();
    applied(&mut client, "added/wire-model").await;

    assert!(client
        .request(set_model(session_id, "missing/bad-model"))
        .await
        .is_err());
    let replay = resume(&mut client, session_id).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|inbound| applied_value(inbound).as_deref() == Some("added/wire-model")),
        "a reattachment announces the applied model: {replay:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_lists_sessions_and_drains_over_the_real_socket() {
    let mut agentd = spawn_agentd();
    let client = connect_acp(connect_with_retry(&agentd.socket_path).await).await;

    let response = client.initialize("test-client").await.unwrap();
    assert_eq!(
        response.protocol_version,
        agent_client_protocol::schema::ProtocolVersion::V2
    );
    let meta: acp::InitializeMeta = acp::read_horizon_meta(response.meta.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(meta.ext_version, acp::HORIZON_ACP_EXT_VERSION);
    assert_eq!(
        meta.binary_id,
        concat!("horizon-agentd/", env!("CARGO_PKG_VERSION"))
    );

    // No sessions yet.
    assert!(list_sessions(&client).await.is_empty());

    client.drain().await;
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(
        status.success(),
        "horizon-agentd should exit 0 after drain, got {status:?}"
    );
}

/// A client at another extension version is refused by `initialize` with
/// an error naming both versions -- and `_horizon/drain` still works
/// without an `initialize`, so the auto-recovery path can restart the
/// daemon at a compatible version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incompatible_version_range_is_rejected_but_drain_still_works() {
    let mut agentd = spawn_agentd();

    let future = acp::HORIZON_ACP_EXT_VERSION + 5;
    let client = connect_acp(connect_with_retry(&agentd.socket_path).await).await;
    let error = client
        .request(initialize_request("future-horizon", future))
        .await
        .expect_err("a different extension version must be rejected");
    assert!(
        error.message.starts_with("horizon ext version mismatch"),
        "{error:?}"
    );
    assert!(error.message.contains(&future.to_string()), "{error:?}");
    assert!(
        error
            .message
            .contains(&acp::HORIZON_ACP_EXT_VERSION.to_string()),
        "{error:?}"
    );
    // The accept loop serves one connection at a time.
    drop(client);

    drain_uninitialized(connect_with_retry(&agentd.socket_path).await).await;
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(
        status.success(),
        "horizon-agentd should exit 0 after a post-rejection drain, got {status:?}"
    );
}

/// `session/new` -> `session/prompt` -> the resulting updates arrive in
/// the order the mock provider produced them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_agent_then_user_message_streams_events_in_order() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
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

/// `session/list` reflects a session created via `session/new` on the same
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_agents_reflects_live_sessions_after_new_agent() {
    let agentd = spawn_agentd();
    let client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    let created = open_session(&client, session_new(session_id)).await;
    let meta: acp::SessionInfoMeta = acp::read_horizon_meta(created.meta.as_ref())
        .unwrap()
        .unwrap();
    assert_eq!(meta.provider_id, mock_provider_id().0);

    let sessions = list_sessions(&client).await;
    assert_listed(&sessions, session_id, None, Some(test_cwd()));
    assert_eq!(sessions[0].cwd, v2::AbsolutePath::new(test_cwd()));
}

/// An auto-allow *host* tool (`workspace.snapshot`) executes agentd-side
/// but can't answer itself -- it round-trips a `_horizon/host_tool`
/// request (guardrail 4) and folds the client's answer into the same
/// finished tool call an ordinary auto tool would produce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_tool_executes_agentd_side_via_host_tool_round_trip() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "please take a snapshot").await;

    let mut collected = collect_until(&mut client, |inbound| {
        matches!(inbound, Inbound::HostTool(..))
    })
    .await;
    let Some(Inbound::HostTool(request, responder)) = collected.pop() else {
        unreachable!()
    };
    assert_eq!(request.tool_id, "workspace.snapshot");
    responder
        .respond(acp::HostToolResponse {
            output: serde_json::json!({ "tab_count": 1 }),
        })
        .unwrap();

    collected.extend(
        collect_until(&mut client, |inbound| {
            finished_tool(inbound).is_some_and(|(call, _)| {
                call.raw_output
                    .as_opt_ref()
                    .flatten()
                    .is_some_and(|output| output["tab_count"] == 1)
            })
        })
        .await,
    );
    assert!(
        collected.iter().filter_map(tool_update).any(|call| {
            call.title == MaybeUndefined::Value("workspace.snapshot".to_string())
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
) -> (acp::ApprovalMeta, Vec<Inbound>) {
    let session_id = SessionId::new();
    open_session(client, session_new(session_id)).await;
    prompt(client, session_id, text).await;

    let mut collected =
        collect_until(client, |inbound| matches!(inbound, Inbound::Permission(..))).await;
    let (request, responder) = take_permission(&mut collected);
    let approval = approval_meta(&request);
    assert_eq!(request.session_id, acp_id(session_id));
    let requested = collected
        .iter()
        .filter_map(tool_update)
        .find(|call| has_status(call, v2::ToolCallStatus::Pending))
        .expect("tool request before approval");
    assert_eq!(&*requested.tool_call_id.0, approval.occurrence_id);
    assert_eq!(tool_meta(requested).unwrap().call_id, approval.call_id);
    let Some(v2::RequestPermissionSubject::ToolCall(subject)) = &request.subject else {
        panic!("the permission names its tool call: {request:?}");
    };
    assert_eq!(&*subject.tool_call.tool_call_id.0, approval.occurrence_id);

    responder.respond(approve()).unwrap();
    let occurrence = approval.occurrence_id.clone();
    let collected = collect_until(client, |inbound| {
        finished_tool(inbound).is_some_and(|(call, _)| *call.tool_call_id.0 == *occurrence)
    })
    .await;
    assert!(
        collected.iter().filter_map(tool_update).any(|call| {
            *call.tool_call_id.0 == *occurrence && has_status(call, v2::ToolCallStatus::InProgress)
        }),
        "approving should have started the tool call before finishing it, got: {collected:?}"
    );
    (approval, collected)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_round_trip_request_out_approve_in_result_event_out() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;
    let (approval, collected) = approve_and_finish(&mut client, "please run a tool").await;
    let (_, meta) = collected.iter().rev().find_map(finished_tool).unwrap();
    assert_eq!(meta.call_id, approval.call_id);
}

/// `bash` runs agentd-side: approving a real `bash` tool call spawns an
/// actual subprocess in agentd, and the result arrives back as the call's
/// final update.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_runs_agentd_side_and_reports_its_result_over_the_wire() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;
    let (approval, collected) = approve_and_finish(&mut client, "please run bash").await;
    let (call, meta) = collected.iter().rev().find_map(finished_tool).unwrap();
    assert_eq!(meta.call_id, approval.call_id);
    assert_eq!(meta.tool_id, "bash");
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
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "please run bash").await;
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
            .any(|(call, _)| *call.tool_call_id.0 == *occurrence)
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
        .filter(|(call, _)| *call.tool_call_id.0 == *occurrence)
        .count();
    assert_eq!(
        finished, 1,
        "a duplicate approval must never produce a second result, got: {replay:?}"
    );

    let mut report = None;
    for _ in 0..100 {
        let candidate = horizon_agent::persistence::event_log::read(&agentd.event_log_path)
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

/// The mock provider's `"streaming tool"` trigger emits ephemeral
/// tool-call-progress ticks before the real tool call -- these must reach
/// a connected client (as `_horizon/tool_call_progress`) and must never
/// appear in the durable on-disk event log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_tool_call_progress_reaches_the_client_but_never_the_event_log() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "please use the streaming tool").await;

    let collected = collect_until(&mut client, |inbound| {
        tool_update(inbound).is_some_and(|call| has_status(call, v2::ToolCallStatus::Pending))
    })
    .await;
    let requested = collected.iter().rev().find_map(tool_update).unwrap();
    assert_eq!(
        requested.title,
        MaybeUndefined::Value("mock.approval_required".to_string())
    );
    let progress_ticks: Vec<usize> = collected
        .iter()
        .filter_map(|inbound| match inbound {
            Inbound::ToolCallProgress(acp::ToolCallProgressNotification {
                event: acp::ToolCallProgressEvent::Progress { bytes, .. },
                ..
            }) => Some(*bytes),
            _ => None,
        })
        .collect();
    assert!(
        progress_ticks.len() >= 3,
        "expected every mock streaming tick to reach the client, got: {progress_ticks:?}"
    );
    assert!(
        progress_ticks.windows(2).all(|pair| pair[1] >= pair[0]),
        "byte counts should grow monotonically as the mock provider streams, got: {progress_ticks:?}"
    );

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
        "the persisted event log must never contain the ephemeral tool-call-progress preview, got:\n{log_contents}"
    );
}

/// A corrupt line found during startup must be reported to a connecting
/// client once, as a `_horizon/session_event` -- not just printed to stderr
/// -- so Horizon's status bar can surface it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_event_log_lines_are_reported_to_the_client_once_per_connection() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let event_log_path = std::env::temp_dir().join(format!(
        "horizon-agentd-e2e-events-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&event_log_path, b"not valid json\n{\"text\":\"\xe6\x97")
        .expect("write corrupt line and a tail split inside UTF-8");

    let agentd = spawn_agentd_at(socket_path, event_log_path);
    let mut client = connect(&agentd.socket_path).await;

    let collected = collect_until(&mut client, |inbound| {
        matches!(
            inbound,
            Inbound::SessionEvent(acp::SessionEventNotification::SkippedLines { .. })
        )
    })
    .await;
    let Some(Inbound::SessionEvent(acp::SessionEventNotification::SkippedLines { summary })) =
        collected.last()
    else {
        unreachable!()
    };
    assert_eq!(summary, "skipped 1 corrupt line and a torn trailing line");

    // The ignored tail must not consume the first new record after startup.
    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "after torn tail").await;
    collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: after torn tail")
    })
    .await;
    wait_for_persisted_event(&agentd.event_log_path, session_id, |event| {
        matches!(event, Event::MessageCommitted(message)
            if message.role == MessageRole::Assistant && message.text == "Mock response: after torn tail")
    }).await;
    let report = horizon_agent::persistence::event_log::read(&agentd.event_log_path).unwrap();
    assert_eq!(
        report.corrupt_line_count, 1,
        "only the original corrupt line remains"
    );
    assert!(!report.ignored_partial_line);
    assert_eq!(report.records.first().unwrap().sequence, 0);
    assert!(report.records.iter().any(|record| matches!(&record.event,
        Event::MessageCommitted(message) if message.role == MessageRole::User && message.text == "after torn tail"
    )));
}

/// Step 4's headline scenario: `kill -9` a live daemon mid-session (a turn
/// genuinely still open, waiting for approval), respawn against the same
/// log, and confirm replay: transcript survives, the interrupted turn is
/// committed as cancelled, the session is immediately usable again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_agentd_respawns_and_replays_transcript_with_open_turn_cancelled() {
    let agentd = spawn_agentd();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let mut client = connect(&socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "please run a tool").await;
    collect_until(&mut client, |inbound| {
        matches!(inbound, Inbound::Permission(..))
    })
    .await;
    wait_for_persisted_event(&event_log_path, session_id, |event| {
        matches!(event, Event::ApprovalRequested(_))
    })
    .await;

    let interrupted_turn_id = horizon_agent::persistence::event_log::read(&event_log_path)
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

    let mut respawned = spawn_agentd_at(socket_path, event_log_path);
    let mut client = connect(&respawned.socket_path).await;

    assert_listed(
        &list_sessions(&client).await,
        session_id,
        None,
        Some(test_cwd()),
    );

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
                .any(|(call, _)| call.tool_call_id == requested.tool_call_id),
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
    assert!(collected
        .iter()
        .any(|inbound| user_message(inbound).as_deref() == Some("hello again")));

    wait_for_persisted_event(&respawned.event_log_path, session_id, |event| {
        matches!(event, Event::MessageCommitted(message)
            if message.role == MessageRole::Assistant && message.text == "Mock response: hello again")
    })
    .await;
    client.drain().await;
    assert!(wait_for_exit(&mut respawned.child).await.success());

    let records = horizon_agent::persistence::event_log::read(&respawned.event_log_path)
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

/// A crash-and-respawn must restore a session's role, not just its
/// provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_restores_the_sessions_role_after_a_crash_and_respawn() {
    let agentd = spawn_agentd();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let mut client = connect(&socket_path).await;

    let session_id = SessionId::new();
    open_session(
        &client,
        new_session_request(
            session_id,
            &mock_provider_id().0,
            Some("config"),
            test_cwd(),
            false,
        ),
    )
    .await;
    // Drain the startup burst until its init message reaches the wire...
    collect_until(&mut client, |inbound| {
        agent_message(inbound).is_some() || user_message(inbound).is_some()
    })
    .await;
    // ...and disk, before the hard kill.
    wait_for_persisted_event(&event_log_path, session_id, |event| {
        matches!(event, Event::MessageCommitted(_))
    })
    .await;

    drop(client);
    agentd.kill_and_wait();

    let respawned = spawn_agentd_at(socket_path, event_log_path);
    let client = connect(&respawned.socket_path).await;
    assert_listed(
        &list_sessions(&client).await,
        session_id,
        Some("config"),
        Some(test_cwd()),
    );
}

/// A daemon restart must preserve an isolated session's authoritative root
/// and tier-1 eligibility. The regression this pins down downgraded every
/// resumed session to the daemon cwd with `isolated=false`, so later bash
/// calls escaped the per-command sandbox and requested manual approval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_re_adopts_an_isolated_worktree_and_keeps_bash_contained() {
    let repo = isolated_session_fixture();
    let agentd = spawn_agentd();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let client = connect(&socket_path).await;
    let session_id = SessionId::new();
    open_session(
        &client,
        new_session_request(
            session_id,
            &mock_provider_id().0,
            None,
            repo.path().to_path_buf(),
            true,
        ),
    )
    .await;

    let mut isolated_root = None;
    for _ in 0..200 {
        if let Some(root) = list_sessions(&client)
            .await
            .iter()
            .find(|info| info.session_id == acp_id(session_id))
            .and_then(|info| info_meta(info).workspace_root)
            .filter(|root| root != repo.path())
        {
            isolated_root = Some(root);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let isolated_root = isolated_root.expect("isolated worktree resolution timed out");
    assert!(
        isolated_root.starts_with(repo.path().join(".horizon/worktrees")),
        "resolved root should be the session-owned worktree: {}",
        isolated_root.display()
    );

    wait_for_persisted_event(&event_log_path, session_id, |event| {
        matches!(event, Event::StateChanged(SessionState::WaitingForUser))
    })
    .await;
    let report = horizon_agent::persistence::event_log::read(&event_log_path)
        .expect("the session context should be readable before restart");
    assert!(report.records.iter().any(|record| {
        record.session_id == session_id
            && record.session_context.as_ref().is_some_and(|context| {
                context.workspace_root.as_ref() == Some(&isolated_root) && context.isolated_worktree
            })
    }));

    drop(client);
    agentd.kill_and_wait();

    let respawned = spawn_agentd_at(socket_path, event_log_path);
    let mut client = connect(&respawned.socket_path).await;
    let resumed = list_sessions(&client)
        .await
        .into_iter()
        .find(|info| info.session_id == acp_id(session_id))
        .expect("the isolated session should resume live");
    assert_eq!(
        info_meta(&resumed).workspace_root.as_ref(),
        Some(&isolated_root)
    );

    let _ = resume(&mut client, session_id).await.unwrap();
    if !horizon_sandbox::is_available() {
        return;
    }

    prompt(&client, session_id, "please run bash").await;
    let collected = collect_until(&mut client, |inbound| {
        finished_tool(inbound).is_some_and(|(_, meta)| meta.call_id == "mock-bash-1")
    })
    .await;
    assert!(
        !collected
            .iter()
            .any(|inbound| matches!(inbound, Inbound::Permission(..))),
        "a resumed isolated session should retain tier-1 auto execution: {collected:?}"
    );
    let (call, meta) = collected.iter().rev().find_map(finished_tool).unwrap();
    let output = call.raw_output.as_opt_ref().flatten().unwrap();
    assert_eq!(
        meta.auto_approved,
        Some(true),
        "resumed call should retain the contained classification: {output:?}"
    );
    assert_eq!(
        meta.policy_tier.as_deref(),
        Some("contained"),
        "resumed call should retain tier-1 eligibility: {output:?}"
    );
}

/// `session/resume` bootstrap (no crash): a client that disconnects and
/// reconnects to the same running daemon must receive exactly the updates
/// it had seen live, message ids included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_agent_after_reconnect_rebuilds_an_equivalent_frame() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "hello").await;

    let mut seen_reply = false;
    let live = collect_until(&mut client, |inbound| {
        if agent_message(inbound).as_deref() == Some("Mock response: hello") {
            seen_reply = true;
        }
        seen_reply && idle_stop_reason(inbound).is_some()
    })
    .await;

    // Disconnect without draining -- the session keeps running.
    drop(client);

    let mut client = connect(&agentd.socket_path).await;
    let replayed = resume(&mut client, session_id).await.unwrap();
    assert_eq!(
        replayed.iter().map(wire).collect::<Vec<_>>(),
        live.iter().map(wire).collect::<Vec<_>>(),
        "session/resume must replay exactly what the live connection saw"
    );
}

/// The server-side substance of `Reload Agent Runtime`: drain a live
/// session gracefully (not a crash), respawn against the same paths, and
/// confirm the session survives with its transcript intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drained_agentd_respawns_and_preserves_a_completed_session() {
    let mut agentd = spawn_agentd();
    let socket_path = agentd.socket_path.clone();
    let event_log_path = agentd.event_log_path.clone();
    let mut client = connect(&socket_path).await;

    let session_id = SessionId::new();
    open_session(&client, session_new(session_id)).await;
    prompt(&client, session_id, "hello").await;
    collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: hello")
    })
    .await;

    client.drain().await;
    drop(client);
    let status = wait_for_exit(&mut agentd.child).await;
    assert!(status.success(), "drain should exit 0, got {status:?}");

    let respawned = spawn_agentd_at(socket_path, event_log_path);
    let mut client = connect(&respawned.socket_path).await;
    assert_listed(
        &list_sessions(&client).await,
        session_id,
        None,
        Some(test_cwd()),
    );

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
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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

    let agentd = spawn_agentd_at(socket_path, event_log_path);
    let client = connect(&agentd.socket_path).await;
    assert_listed(&list_sessions(&client).await, live_session, None, None);
}

/// Fix 1: `initialize` must answer well before a slow resume finishes, and
/// `session/list` must wait for it -- proven with the resume-delay hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_answers_immediately_while_session_list_waits_for_a_slow_resume() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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
    let agentd = spawn_agentd_with_resume_delay(socket_path, event_log_path, RESUME_DELAY_MS);

    let initialize_started = Instant::now();
    let client = connect(&agentd.socket_path).await;
    let initialize_elapsed = initialize_started.elapsed();
    assert!(
        initialize_elapsed < Duration::from_millis(RESUME_DELAY_MS / 2),
        "initialize should answer well before the artificial resume delay elapses, took {initialize_elapsed:?}"
    );

    let list_started = Instant::now();
    let sessions = list_sessions(&client).await;
    let list_elapsed = list_started.elapsed();
    assert!(
        list_elapsed >= Duration::from_millis(RESUME_DELAY_MS) - Duration::from_millis(300),
        "session/list should have waited for the (artificially slow) resume to finish, took {list_elapsed:?}"
    );
    assert_listed(&sessions, live_session, None, None);
}

/// Fix 1's other half: a second daemon against a live socket must bail
/// before reading its own log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_agentd_against_a_live_socket_exits_before_reading_its_own_log() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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

    let first = spawn_agentd_at(socket_path.clone(), event_log_path.clone());
    // Wait for the first instance to be up and resumed (session/list's own
    // readiness gate) before racing a second one against it.
    let client = connect(&first.socket_path).await;
    let _ = list_sessions(&client).await;
    drop(client);

    // Spawned as a bare `Child` rather than an `AgentdProcess`: this one is
    // expected to bail out immediately, and its socket/event log belong to
    // the *first* process, which must outlive it -- so nothing here may own
    // those paths' cleanup.
    let second_paths =
        AgentdPaths::scratch_at("agentd-e2e", socket_path.clone(), event_log_path.clone());
    let mut second_command = agentd_hermetic_command(&resolve_agentd_binary(), &second_paths);
    second_command
        .env_remove(TEST_RESUME_DELAY_MS_VAR)
        .stderr(Stdio::piped());
    let mut second = spawn_with_link_retry(&mut second_command);

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

/// Task 1: `initialize`/`session/list` must both answer promptly even
/// while an (artificially slowed) DuckDB rebuild is still running in the
/// background.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duckdb_rebuild_delay_does_not_block_initialize_or_session_list() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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
    let agentd =
        spawn_agentd_with_duckdb_rebuild_delay(socket_path, event_log_path, REBUILD_DELAY_MS);

    let initialize_started = Instant::now();
    let client = connect(&agentd.socket_path).await;
    let initialize_elapsed = initialize_started.elapsed();
    assert!(
        initialize_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "initialize should answer well before the artificial duckdb rebuild delay elapses, took {initialize_elapsed:?}"
    );

    let list_started = Instant::now();
    let sessions = list_sessions(&client).await;
    let list_elapsed = list_started.elapsed();
    assert!(
        list_elapsed < Duration::from_millis(REBUILD_DELAY_MS / 2),
        "session/list must not wait on the (slow) duckdb rebuild, took {list_elapsed:?}"
    );
    assert_listed(&sessions, live_session, None, None);
}

/// Task 2's skip path: a second spawn against an *unchanged* event log must
/// skip the DuckDB rebuild entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_log_skips_duckdb_rebuild_on_respawn() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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

    let first = spawn_agentd_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    drop(connect(&first.socket_path).await);
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

    let second = spawn_agentd_with_duckdb_options(socket_path, event_log_path, state_db_path);
    drop(connect(&second.socket_path).await);
    second
        .wait_for_stderr_line("DuckDB projection already current, skipping rebuild")
        .await;
}

/// Task 2's other half: a log that grew since the projection was last built
/// must still be reconciled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_log_triggers_duckdb_rebuild_on_respawn() {
    let socket_path = std::env::temp_dir().join(format!(
        "hzn-e2e-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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

    let first = spawn_agentd_with_duckdb_options(
        socket_path.clone(),
        event_log_path.clone(),
        state_db_path.clone(),
    );
    drop(connect(&first.socket_path).await);
    first
        .wait_for_stderr_line("DuckDB projection rebuilt (")
        .await;
    first.kill_and_wait();

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

    let second = spawn_agentd_with_duckdb_options(socket_path, event_log_path, state_db_path);
    drop(connect(&second.socket_path).await);
    let catch_up_line = second
        .wait_for_stderr_line("DuckDB projection caught up incrementally (")
        .await;
    assert!(
        !catch_up_line.contains("already current"),
        "a stale (grown) log must trigger real reconciliation work, not the skip path: {catch_up_line}"
    );
}

// --- the Mixture-of-Agents real-provider smoke test ------------------------
//
// Everything above this line runs against the mock provider or the
// deterministic fallback. This one talks to a real endpoint with a real
// key, so it is `#[ignore]`d (the gate and the `sandboxed` profile never
// select it) and additionally gated on two environment variables, because
// `cargo nextest run --run-ignored all` would otherwise spend money.
//
//   HORIZON_MOA_SMOKE=1 OPENAI_API_KEY=... \
//     cargo nextest run -p horizon-agentd --run-ignored only \
//       --no-capture moa_pass_runs_against_a_real_provider

/// Set to `1` to let the smoke test below actually run.
const MOA_SMOKE_VAR: &str = "HORIZON_MOA_SMOKE";

/// The `[[providers]]` entry the fixture config defines, and the three
/// models the pass runs on. The aggregator is the first proposer's model
/// too, which is the paper's "one member sampled twice" setting and also
/// proves the per-session model pin is not just "whatever the entry
/// defaults to".
const SMOKE_PROVIDER: &str = "synthetic";
const SMOKE_BASE_URL: &str = "https://api.synthetic.new/openai/v1";
const SMOKE_AGGREGATOR_MODEL: &str = "hf:deepseek-ai/DeepSeek-V4.1-Flash";
const SMOKE_PROPOSER_MODELS: [&str; 3] = [
    "hf:deepseek-ai/DeepSeek-V4.1-Flash",
    "hf:zai-org/GLM-5.3-Flash",
    "hf:Qwen/Qwen3.8-27B",
];

/// The fact the fixture hides. Not derivable from anything else in the
/// tree, so an answer carrying it proves a proposer read the file.
const SMOKE_SECRET_VALUE: &str = "48213";

/// A MoA pass end to end against the configured provider: three proposer
/// sessions each investigate the fixture with `fs.*`, and the aggregator
/// answers from their reports.
///
/// What only a real provider can show, and what this exists for:
///
/// * each proposer's `provider_request_sent` names the model the `[[moa]]`
///   entry pinned for it, not the `[[providers]]` entry's default — the
///   model pin reaching the wire is otherwise only structural;
/// * the aggregator's answer carries a value that exists nowhere but the
///   fixture file, so the proposers really did read it and the aggregator
///   really did use what they returned;
/// * the injected proposal block stays out of the aggregator's committed
///   history.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spends real provider credit; run explicitly with HORIZON_MOA_SMOKE=1"]
async fn moa_pass_runs_against_a_real_provider() {
    if std::env::var(MOA_SMOKE_VAR).as_deref() != Ok("1") {
        println!("skipped: set {MOA_SMOKE_VAR}=1 to run the MoA real-provider smoke test");
        return;
    }
    if std::env::var_os("OPENAI_API_KEY").is_none() {
        println!("skipped: OPENAI_API_KEY is not set, so the provider cannot be reached");
        return;
    }

    let fixture = write_smoke_fixture();
    let config_path = write_smoke_config();
    let paths = AgentdPaths::scratch("agentd-moa-smoke");
    let event_log_path = paths.event_log_path.clone();
    let agentd = AgentdSpawn::new(resolve_agentd_binary(), paths)
        .env_remove(TEST_RESUME_DELAY_MS_VAR)
        .env_remove(TEST_DUCKDB_REBUILD_DELAY_MS_VAR)
        // Overrides the hermetic contract's deliberately-missing config
        // with this test's own; every other isolated path stays as the
        // contract set it.
        .env("HORIZON_CONFIG", &config_path)
        .spawn();
    let mut client = connect(&agentd.socket_path).await;

    let session_id = SessionId::new();
    client
        .request(new_session_request(
            session_id,
            &horizon_agent::registry::moa_provider_id("mix").0,
            None,
            fixture.clone(),
            false,
        ))
        .await
        .expect("the [[moa]] entry must be registered as a provider");

    let question = "What is the value of RETRY_BUDGET_MS in this repository, and which file \
                    and line defines it? Answer with the number and the path."
        .to_string();
    let started = Instant::now();
    prompt(&client, session_id, &question).await;

    // One pass runs three sessions to completion before the aggregator's
    // own turn starts, so the wait is long.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("the MoA pass did not finish within 10 minutes");
        let inbound = client.next(remaining).await;
        if let Some(Some(reason)) = idle_stop_reason(&inbound) {
            assert_eq!(
                reason,
                v2::StopReason::EndTurn,
                "the aggregator's turn must complete"
            );
            break;
        }
    }
    let elapsed = started.elapsed();

    // Drain first: the assertions read the event log from disk, and a
    // graceful drain is what flushes the writer's queue.
    client.drain().await;
    let report = horizon_agent::persistence::event_log::read(&event_log_path)
        .expect("the isolated event log must be readable");
    let records = report.records;

    // -- 1. one pass record, naming the three configured members ----------
    let passes: Vec<&horizon_agent::contract::MoaPassStarted> = records
        .iter()
        .filter_map(|record| match &record.event {
            Event::MoaPassStarted(pass) => Some(pass),
            _ => None,
        })
        .collect();
    assert_eq!(
        passes.len(),
        1,
        "one owner message must open exactly one pass, got {}",
        passes.len()
    );
    let pass = passes[0];
    assert_eq!(pass.entry, "mix");
    assert_eq!(
        pass.proposers
            .iter()
            .map(|proposer| (proposer.provider.as_str(), proposer.model.as_str()))
            .collect::<Vec<_>>(),
        SMOKE_PROPOSER_MODELS
            .iter()
            .map(|model| (SMOKE_PROVIDER, *model))
            .collect::<Vec<_>>(),
        "the record must name every configured member, in order"
    );

    // -- 2/3. every request carries the model that session was pinned to --
    let mut summaries = Vec::new();
    for (position, proposer) in pass.proposers.iter().enumerate() {
        let stats = session_stats(&records, proposer.session_id);
        assert!(
            !stats.request_models.is_empty(),
            "proposer {} ({}) issued no provider request",
            position + 1,
            proposer.model
        );
        for model in &stats.request_models {
            assert_eq!(
                model,
                &proposer.model,
                "proposer {} must run the model the entry pinned for it, not the entry's \
                 default; its requests named {model}",
                position + 1
            );
        }
        assert!(
            stats.tool_calls > 0,
            "proposer {} ({}) answered without reading anything",
            position + 1,
            proposer.model
        );
        summaries.push((format!("proposer {}", position + 1), stats));
    }

    let aggregator = session_stats(&records, session_id);
    assert!(
        !aggregator.request_models.is_empty(),
        "the aggregator issued no provider request"
    );
    for model in &aggregator.request_models {
        assert_eq!(
            model, SMOKE_AGGREGATOR_MODEL,
            "the aggregator must run the entry's aggregator model, its requests named {model}"
        );
    }

    // -- 4. the answer carries what only the fixture knows -----------------
    let answer = aggregator
        .final_assistant_text
        .clone()
        .expect("the aggregator committed no assistant message");
    assert!(
        answer.contains(SMOKE_SECRET_VALUE),
        "the answer must carry the value only the fixture defines ({SMOKE_SECRET_VALUE}); \
         got: {answer}"
    );

    // -- 5. no proposal ever entered the aggregator's history --------------
    for text in &aggregator.committed_texts {
        assert!(
            !text.contains("--- Answer 1 (session_id"),
            "the injected proposal block must never be committed to history; found it in: {text}"
        );
        for proposer in &pass.proposers {
            let id = proposer.session_id.as_uuid().to_string();
            assert!(
                !text.contains(&id),
                "a proposer session id leaked into the aggregator's history: {text}"
            );
        }
    }

    summaries.push(("aggregator".to_string(), aggregator.clone()));

    // -- the summary ------------------------------------------------------
    println!(
        "\n=== MoA smoke: {} in {:.1}s ===",
        pass.entry,
        elapsed.as_secs_f64()
    );
    println!("question: {question}");
    for (role, stats) in &summaries {
        println!(
            "{role:<12} model={:<40} requests={:<3} tool_calls={:<3} in={:<8} out={:<7} \
             cached={:<8} {:.1}s",
            stats
                .request_models
                .first()
                .map(String::as_str)
                .unwrap_or("-"),
            stats.request_models.len(),
            stats.tool_calls,
            stats.input_tokens,
            stats.output_tokens,
            stats.cached_input_tokens,
            stats.wall_seconds,
        );
    }
    for (position, proposer) in pass.proposers.iter().enumerate() {
        let stats = session_stats(&records, proposer.session_id);
        let text = stats.final_assistant_text.unwrap_or_default();
        let head: String = text.chars().take(300).collect();
        println!(
            "\n--- proposal {} ({}) ---\n{head}",
            position + 1,
            proposer.model
        );
    }
    println!("\n--- aggregator answer ---\n{answer}\n");

    // Reported, never failed: the instruction asks the aggregator not to
    // reveal the mechanism, and a model that ignores it is a prompt finding
    // rather than a broken pass.
    for leak in ["assistants", "proposals"] {
        if answer.to_ascii_lowercase().contains(leak) {
            println!("NOTE: the answer mentions {leak:?} — the aggregator revealed the mechanism");
        }
    }
}

/// What one session did, folded out of the event log.
#[derive(Clone, Debug, Default)]
struct SessionStats {
    /// The model named by every `provider_request_sent`, in order.
    request_models: Vec<String>,
    tool_calls: usize,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    wall_seconds: f64,
    /// Every assistant-role message text, for the history assertions.
    committed_texts: Vec<String>,
    /// The last of them — the session's answer.
    final_assistant_text: Option<String>,
}

fn session_stats(
    records: &[horizon_agent::persistence::event_log::Record],
    session_id: SessionId,
) -> SessionStats {
    let mut stats = SessionStats::default();
    let mut first_ms = None;
    let mut last_ms = 0;
    for record in records.iter().filter(|r| r.session_id == session_id) {
        first_ms.get_or_insert(record.created_at_unix_ms);
        last_ms = record.created_at_unix_ms;
        match &record.event {
            Event::ProviderRequestSent(sent) => stats.request_models.push(sent.model.clone()),
            Event::ToolCallRequested(_) => stats.tool_calls += 1,
            Event::ProviderRequestUsage(usage) => {
                stats.input_tokens += usage.input_tokens;
                stats.output_tokens += usage.output_tokens;
                stats.cached_input_tokens += usage.cached_input_tokens;
            }
            Event::MessageCommitted(message) if message.role == MessageRole::Assistant => {
                stats.committed_texts.push(message.text.clone());
                stats.final_assistant_text = Some(message.text.clone());
            }
            Event::MessageCommitted(message) => stats.committed_texts.push(message.text.clone()),
            _ => {}
        }
    }
    // The session's own startup notice is an assistant message; it is never
    // the answer.
    if stats
        .final_assistant_text
        .as_deref()
        .is_some_and(|text| text.starts_with("Rig provider `"))
    {
        stats.final_assistant_text = None;
    }
    stats.wall_seconds = (last_ms.saturating_sub(first_ms.unwrap_or(last_ms))) as f64 / 1000.0;
    stats
}

/// The config the smoke daemon loads: one openai-compatible entry and one
/// `[[moa]]` entry over it. No `models` alias map — a `[[providers]]` entry
/// needs none, and `[[moa]]` members name model ids directly.
fn write_smoke_config() -> PathBuf {
    let path = horizon_daemon_testkit::scratch_file("agentd-moa-smoke-config", "toml");
    let proposers = SMOKE_PROPOSER_MODELS
        .iter()
        .map(|model| format!("  {{ provider = \"{SMOKE_PROVIDER}\", model = \"{model}\" }},"))
        .collect::<Vec<_>>()
        .join("\n");
    // `default_provider` is a top-level key, so it has to precede the first
    // array-of-tables header; after one, TOML reads it as a key of that
    // table instead.
    let contents = format!(
        "default_provider = \"{SMOKE_PROVIDER}\"\n\
         \n\
         [[providers]]\n\
         name = \"{SMOKE_PROVIDER}\"\n\
         base_url = \"{SMOKE_BASE_URL}\"\n\
         \n\
         [[moa]]\n\
         name = \"mix\"\n\
         aggregator = {{ provider = \"{SMOKE_PROVIDER}\", model = \"{SMOKE_AGGREGATOR_MODEL}\" }}\n\
         proposers = [\n{proposers}\n]\n"
    );
    std::fs::write(&path, contents).expect("the smoke config must be writable");
    path
}

/// A small tree whose only interesting content is one constant nothing else
/// in the world knows, plus decoys so finding it takes a real search.
fn write_smoke_fixture() -> PathBuf {
    let root = horizon_daemon_testkit::scratch_file("agentd-moa-smoke-fixture", "dir");
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("the smoke fixture must be writable");
    std::fs::write(
        root.join("README.md"),
        "# smoke-fixture\n\nA tiny crate used by Horizon's MoA smoke test.\n\
         Tunables live under `src/`.\n",
    )
    .unwrap();
    std::fs::write(
        src.join("limits.rs"),
        "//! Retry and backoff tunables.\n\n\
         /// How long a single request may keep retrying before it is given up on.\n\
         pub const RETRY_BUDGET_MS: u64 = 48213;\n\n\
         pub const BACKOFF_STEP_MS: u64 = 250;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("timeouts.rs"),
        "//! Decoy: connect/read timeouts, deliberately unrelated to the retry budget.\n\n\
         pub const CONNECT_TIMEOUT_MS: u64 = 3000;\n\
         pub const READ_TIMEOUT_MS: u64 = 9000;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub mod limits;\npub mod timeouts;\n\n\
         /// Decoy: names a budget but defines none.\n\
         pub fn retry_budget() -> u64 {\n    limits::RETRY_BUDGET_MS\n}\n",
    )
    .unwrap();
    root
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
    let (writer, ready) = WriterHandle::open(&paths.event_log_path);
    assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
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
            response_id:"interrupted-response".into(),identity:identity.clone(),codec:1,
            tool_call:serde_json::json!({"id":"pending","function":{"name":"fs.read","arguments":{"path":"never-dispatched"}},"signature":"signed-call"}).into(),
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
    let socket = daemon.socket_path.clone();
    let log = daemon.event_log_path.clone();
    for round in 0..2 {
        let mut client = connect(&socket).await;
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
        client.drain().await;
        drop(client);
        assert!(wait_for_exit(&mut daemon.child).await.success());
        let records = horizon_agent::persistence::event_log::read(&log)
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
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;
    let session = SessionId::new();
    open_session(&client, session_new(session)).await;
    prompt(&client, session, &format!("slow {}", "word ".repeat(100))).await;
    collect_until(&mut client, is_chunk).await;
    for _ in 0..4 {
        let history = resume(&mut client, session).await.unwrap();
        assert!(history.iter().any(is_chunk));
    }
    let mut messages = resume(&mut client, session).await.unwrap();
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
    assert!(!messages
        .iter()
        .any(|inbound| { idle_stop_reason(inbound) == Some(Some(v2::StopReason::Cancelled)) }));
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(
        messages.iter().map(wire).collect::<Vec<_>>(),
        replay.iter().map(wire).collect::<Vec<_>>(),
        "streaming attach must preserve the entire ordered sequence"
    );
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
    let socket = temp.path().join("agentd.sock");
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
    let agentd = spawn_agentd_at(socket.clone(), log.clone());
    let mut abandoned = connect(&socket).await;
    abandoned.send_ordered(
        v2::ResumeSessionRequest::new(acp_id(session), test_cwd())
            .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
    );
    let _first = abandoned.next(Duration::from_secs(120)).await;
    drop(abandoned);
    let mut client = connect(&socket).await;
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&replay), history);
    drop(client);
    agentd.kill_and_wait();
    let _restarted = spawn_agentd_at(socket.clone(), log);
    let mut client = connect(&socket).await;
    let restored = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&restored), history);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_isolation_warning_is_visible_and_survives_reattachment() {
    let agentd = spawn_agentd();
    let mut client = connect(&agentd.socket_path).await;
    let session = SessionId::new();
    let plain_directory = tempfile::tempdir().unwrap();
    open_session(
        &client,
        new_session_request(
            session,
            &mock_provider_id().0,
            None,
            plain_directory.path().into(),
            true,
        ),
    )
    .await;
    let collected = collect_until(&mut client, |inbound| {
        session_error(inbound)
            .is_some_and(|message| message.contains("continuing without isolation"))
    })
    .await;
    let warning = session_error(collected.last().unwrap())
        .unwrap()
        .to_string();
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .filter(|inbound| session_error(inbound) == Some(warning.as_str()))
            .count(),
        1
    );
    let records = horizon_agent::persistence::event_log::read(&agentd.event_log_path)
        .unwrap()
        .records;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.session_id == session
                && matches!(&record.event, Event::Error(error) if error.message == warning))
            .count(),
        1
    );
}
