//! The tests about the `horizon-agent-acp` process itself, against the
//! real binary: where it is found, a restart and what survives it, what a
//! corrupt log says on its standard error, worktrees across a restart. How
//! the agent behaves over ACP is tested in this test's process
//! (`tests/agent.rs`). The binary is spawned
//! through `horizon-daemon-testkit`: it owns binary resolution (the runtime
//! `CARGO_BIN_EXE_horizon-agent-acp` env var in preference to the same-named
//! compile-time `env!()` bake -- `docs/tasks/backlog.md` #40), the
//! spawn-with-retry for cargo's uplift window (#36), and this daemon's
//! hermetic-spawn contract, which `horizon-terminald`'s suite spawns
//! through too -- see `docs/agent-runtime-split-design.md`'s step 2
//! deliverables.
//!
//! These talk to the binary as an ACP v2 client over its standard input
//! and output, through the testkit's [`AcpClient`].

use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::v2::schema as v2;
use horizon_agent::contract::{
    Event, MessageRole, ProviderId, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::persistence::event_log::{Appender, WriterHandle};
use horizon_agent_host::options as acp;
use horizon_daemon_testkit::{
    AcpClient, AgentAcpPaths, AgentAcpProcess, AgentAcpSpawn, Inbound, cargo_bin_exe_var,
    resolve_daemon_binary, wait_for_exit,
};

mod support;
use support::*;

/// The env var `horizon-agent-acp`'s `main` reads to hold its
/// event-log-read-plus-resume phase until a file is there -- see that
/// binary's own doc comment on the constant of the same name. Test-only;
/// never set outside this file.
const TEST_RESUME_GATE_VAR: &str = "HORIZON_AGENT_TEST_RESUME_GATE";

/// The env var `horizon-agent-acp`'s `main` reads to have the mock provider's
/// slow turns wait, once their first word is out, until a file is there.
/// Test-only; never set outside this file.
const TEST_MOCK_STREAM_GATE_VAR: &str = "HORIZON_AGENT_TEST_MOCK_STREAM_GATE";

/// Resolves the `horizon-agent-acp` binary to spawn. Only the `env!()` bake
/// has to be produced here (that macro expands only inside the package that
/// owns the `[[bin]]` target); the resolution rule itself, and the write-up
/// of why the runtime env var is preferred over that bake, live in
/// `horizon_daemon_testkit::binary`.
fn resolve_agent_acp_binary() -> PathBuf {
    resolve_daemon_binary("horizon-agent-acp", env!("CARGO_BIN_EXE_horizon-agent-acp"))
}

/// The recipe every spawn below starts from: the testkit's hermetic
/// contract (throwaway event log and DuckDB projection, a deliberately
/// nonexistent config file, neutralized git config) plus this suite's own
/// test hooks explicitly cleared, so a hook set for one spawn can never
/// leak into the next.
fn agent_acp_spawn(paths: AgentAcpPaths) -> AgentAcpSpawn {
    AgentAcpSpawn::new(resolve_agent_acp_binary(), paths)
        .env_remove(TEST_RESUME_GATE_VAR)
        .env_remove(TEST_MOCK_STREAM_GATE_VAR)
}

/// Spawns `horizon-agent-acp` at fresh throwaway paths -- what almost every
/// test below wants.
fn spawn_agent_acp() -> AgentAcpProcess {
    agent_acp_spawn(AgentAcpPaths::scratch("agent-acp-e2e")).spawn()
}

/// Same as [`spawn_agent_acp`], but pointed at caller-chosen paths -- the seam
/// step 4's "kill -9 mid-session, respawn" tests use to bring a *second*
/// process up against the *first* process's own event-log paths,
/// simulating a real restart.
fn spawn_agent_acp_at(sessions_dir: PathBuf) -> AgentAcpProcess {
    agent_acp_spawn(AgentAcpPaths::scratch_at("agent-acp-e2e", sessions_dir)).spawn()
}

/// Connects over the child's stdio and completes `initialize` -- every
/// session-hosting test's entry point.
async fn connect(agent_acp: &mut AgentAcpProcess) -> AcpClient {
    agent_acp.connect().await
}

// --- tests -----------------------------------------------------------------

/// Proves the runtime resolution [`resolve_agent_acp_binary`] prefers
/// actually finds an existing binary, and that it's the same binary the
/// compile-time `env!()` bake would have named -- the mechanism backlog
/// #40's fix relies on: both are cargo's own idea of "the `horizon-agent-acp`
/// binary for this test run", differing only in *when* the value is
/// computed (build time vs. this exact invocation), not *what* it points
/// at, for a normal (non-stale-cache) run like this one.
#[test]
fn resolve_agent_acp_binary_finds_an_existing_binary_via_the_runtime_env_var() {
    let var = cargo_bin_exe_var("horizon-agent-acp");
    let runtime_var = std::env::var(&var)
        .expect("cargo/cargo-nextest must set CARGO_BIN_EXE_horizon-agent-acp at test runtime");
    let runtime_path = PathBuf::from(&runtime_var);
    assert!(
        runtime_path.is_file(),
        "runtime env var {var} = {runtime_var} does not point at an existing file"
    );

    let resolved = resolve_agent_acp_binary();
    assert_eq!(
        resolved, runtime_path,
        "resolve_agent_acp_binary must prefer the runtime env var over the compile-time bake"
    );

    // Deliberately NOT asserted: `resolved == env!("CARGO_BIN_EXE_...")`.
    // Divergence between the runtime var and the compile-time bake is this
    // fix's NORMAL operating mode under the shared build-dir -- it happens
    // whenever the cached test binary was compiled in a sibling worktree
    // (possibly since deleted). An equality assertion here failed the
    // integration gate the very first time that scenario occurred; see
    // `docs/tasks/backlog.md` #40.
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
    let mut agent_acp = agent_acp_spawn(AgentAcpPaths::scratch("agent-acp-effort"))
        .env("HORIZON_CONFIG", &config_path)
        .env_remove("HORIZON_EFFORT_TEST_KEY")
        .spawn();
    let mut client = connect(&mut agent_acp).await;
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
    let status = wait_for_exit(&mut agent_acp.child).await;
    assert!(status.success(), "the drained daemon ended with {status:?}");
    let mut agent_acp = agent_acp.respawn_at_same_paths();
    let mut client = connect(&mut agent_acp).await;
    let replay = resume(&mut client, session_id).await.unwrap();
    if !replay
        .iter()
        .any(|inbound| effort(inbound).is_some_and(|(current, _)| current == "low"))
    {
        announced(&mut client, "low").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_event_log_lines_are_reported_in_the_agents_log() {
    let sessions_dir = std::env::temp_dir().join(format!(
        "horizon-agent-acp-e2e-sessions-{}-{}",
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

    let mut agent_acp = agent_acp_spawn(AgentAcpPaths::scratch_at("agent-acp-e2e", sessions_dir))
        .capture_stderr()
        .spawn();
    let mut client = connect(&mut agent_acp).await;
    agent_acp
        .wait_for_stderr_line(
            "skipped 1 corrupt line and a torn trailing line while reading the session logs",
        )
        .await;

    // The ignored tail must not join the first record the session writes
    // once a client resumes it.
    resume(&mut client, damaged).await.unwrap();
    wait_for_persisted_event(&agent_acp.sessions_dir, damaged, |event| {
        matches!(event, Event::StateChanged(SessionState::Created))
    })
    .await;
    let report =
        horizon_agent::persistence::event_log::read_session(&agent_acp.sessions_dir, damaged)
            .unwrap();
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
async fn a_killed_agent_acp_starts_again_and_replays_transcript_with_open_turn_cancelled() {
    let mut agent_acp = spawn_agent_acp();
    let sessions_dir = agent_acp.sessions_dir.clone();
    let mut client = connect(&mut agent_acp).await;

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
    agent_acp.kill_and_wait();

    // Fresh projection path, so this start rebuilds; stderr is captured to
    // wait for that rebuild below.
    let mut respawned = agent_acp_spawn(AgentAcpPaths::scratch_at("agent-acp-e2e", sessions_dir))
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

/// A restart of the agent must preserve the authoritative root and tier-1
/// eligibility of a session that entered a worktree. The regression this
/// pins down downgraded every resumed session to the daemon cwd with
/// `isolated=false`, so later bash calls escaped the per-command sandbox and
/// requested manual approval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_re_adopts_an_isolated_worktree_and_keeps_bash_contained() {
    let repo = isolated_session_fixture();
    let mut agent_acp = spawn_agent_acp();
    let sessions_dir = agent_acp.sessions_dir.clone();
    let mut client = connect(&mut agent_acp).await;
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
    agent_acp.kill_and_wait();

    let mut respawned = spawn_agent_acp_at(sessions_dir);
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

/// `worktree_enter` end to end: a shared session asks for a worktree, the
/// activation lands at the turn boundary, the client learns the new root
/// live, later writes are confined to it, and the agent's restart resumes the
/// session in the same worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_enter_moves_a_shared_session_and_survives_a_restart() {
    let repo = isolated_session_fixture();
    let head = fixture_git_output(repo.path(), &["rev-parse", "HEAD"]);
    let mut agent_acp = spawn_agent_acp();
    let sessions_dir = agent_acp.sessions_dir.clone();
    let mut client = connect(&mut agent_acp).await;
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
    agent_acp.kill_and_wait();

    let mut respawned = spawn_agent_acp_at(sessions_dir);
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

/// The server-side substance of the agent's process starting again: end a live
/// session gracefully (not a crash), respawn against the same paths, and
/// confirm the session survives with its transcript intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_agent_acp_starts_again_and_keeps_a_completed_session() {
    let mut agent_acp = spawn_agent_acp();
    let sessions_dir = agent_acp.sessions_dir.clone();
    let mut client = connect(&mut agent_acp).await;

    let session_id = open_session(&client, session_new()).await;
    prompt(&client, session_id, "hello").await;
    collect_until(&mut client, |inbound| {
        agent_message(inbound).as_deref() == Some("Mock response: hello")
    })
    .await;

    drop(client);
    let status = wait_for_exit(&mut agent_acp.child).await;
    assert!(status.success(), "drain should exit 0, got {status:?}");

    let mut respawned = spawn_agent_acp_at(sessions_dir);
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

/// A crash between durable announcement and dispatch still leaves a valid
/// conversation after restoration, including another full restart of the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rig_conversation_survives_an_undispatched_call_and_repeated_daemon_restart() {
    use horizon_agent::contract::{
        ConversationInputKind, ConversationRecord, OccurrenceId, ToolCallId, ToolCallIdentity,
        ToolOutcome,
    };
    let paths = AgentAcpPaths::scratch("rig-history-restart");
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
    let mut daemon = agent_acp_spawn(paths)
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
    let mut agent_acp = spawn_agent_acp_at(log.clone());
    let mut abandoned = connect(&mut agent_acp).await;
    abandoned.send_ordered(
        v2::ResumeSessionRequest::new(acp_id(session), test_cwd())
            .replay_from(v2::ReplayFrom::Start(v2::ReplayFromStart::new())),
    );
    let _first = abandoned.next(Duration::from_secs(120)).await;
    // Closing the connection mid-replay ends the process.
    drop(abandoned);
    let mut agent_acp = agent_acp.respawn_at_same_paths();
    let mut client = connect(&mut agent_acp).await;
    let replay = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&replay), history);
    drop(client);
    agent_acp.kill_and_wait();
    let mut restarted = spawn_agent_acp_at(log);
    let mut client = connect(&mut restarted).await;
    let restored = resume(&mut client, session).await.unwrap();
    assert_eq!(user_texts(&restored), history);
}

/// A session forked at the cursor before its second message holds the first
/// exchange and the files as they were then, in a worktree of its own; it
/// goes on by itself while the source keeps its own, and comes back with
/// that branch and worktree after the agent restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_forked_at_a_cursor_goes_on_from_there_and_survives_a_restart() {
    let (_outer, repo) = nested_session_fixture();
    let mut daemon = agent_acp_spawn(AgentAcpPaths::scratch("agent-acp-e2e"))
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

/// A session closed is cleaned up before the close is answered: a client
/// that ends the agent right after the answer finds the session's
/// checkpoints gone (board #111).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_session_is_cleaned_up_before_the_close_is_answered() {
    let (_outer, repo) = nested_session_fixture();
    let mut daemon = agent_acp_spawn(AgentAcpPaths::scratch("agent-acp-e2e"))
        .env_remove("HORIZON_AGENT_TEST_NO_CHECKPOINTS")
        .spawn();
    let mut client = connect(&mut daemon).await;
    let session = open_session(
        &client,
        new_session_request(&mock_provider_id().0, repo.clone()),
    )
    .await;
    prompt_and_settle(&mut client, session, "one").await;
    std::fs::write(repo.join("notes.txt"), "after one\n").unwrap();
    prompt_and_settle(&mut client, session, "two").await;
    let refs = || {
        fixture_git_output(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/horizon/checkpoints/{}/", session.as_uuid()),
            ],
        )
    };
    assert!(
        !refs().trim().is_empty(),
        "the session recorded checkpoints"
    );

    client
        .request(v2::CloseSessionRequest::new(acp_id(session)))
        .await
        .expect("session/close is answered");
    assert_eq!(refs().trim(), "", "the checkpoints went before the answer");
}
