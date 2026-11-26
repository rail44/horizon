use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::json;

use super::*;
use crate::contract::{
    ApprovalKind, ApprovalRequest, Command, Event, ProviderEvent, SessionId, ToolCallId,
    ToolCallRequest, ToolCallResult, ToolPermission,
};
use crate::frame::{AgentFrame, AgentFrameItem};
use crate::live::LiveState;
use crate::tools::execution::{Execution, HostTools};
use crate::tools::state::{
    ToolSessionState, register_session_runtime, session_runtime, unregister_session_runtime,
};
use crate::tools::synchronous as sync_tools;
use crate::tools::test_support::{execute_agent_tool, process_agent_provider_event};

/// A `HostTools` stub for tests that need *some* auto-allow host tool to
/// exercise dispatch/processing plumbing, but don't care about real
/// Horizon-side output — the real `workspace_snapshot` implementation lives
/// in Horizon (`agent::host_tools`) since it reads `Workspace`, a type this
/// crate can't depend on (see `docs/agent-runtime-split-design.md`). Returns
/// a fixed, empty snapshot for `workspace_snapshot` and `None` for anything
/// else, so unrelated tool ids still fall through to `tools::fs`.
struct StubHostTools;

impl HostTools for StubHostTools {
    fn execute_auto(&self, tool_id: &str, _input: &serde_json::Value) -> Option<serde_json::Value> {
        match tool_id {
            "workspace_snapshot" => Some(json!({ "stub": true })),
            _ => None,
        }
    }
}

fn temp_workspace(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("horizon-fs-tools-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create temp workspace dir");
    dir.canonicalize().expect("canonicalize temp workspace dir")
}

fn dummy_tool_state() -> ToolSessionState {
    ToolSessionState::new(std::env::temp_dir())
}

/// A throwaway sender for tests that register a session runtime but never
/// exercise the `bash` tool (which is what actually reads from the paired
/// receiver — see `horizon-agent-host`'s session loop,
/// `crates/horizon-agent-host/src/session/`).
fn dummy_bash_results() -> crossbeam_channel::Sender<BashCompletion> {
    crossbeam_channel::unbounded().0
}

/// Forces a file's mtime to a value distinct from whatever the filesystem
/// just recorded, so staleness tests don't depend on timestamp resolution.
fn bump_mtime(path: &Path) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open file to bump mtime");
    file.set_modified(SystemTime::now() + Duration::from_secs(120))
        .expect("set mtime");
}

fn tool_output(execution: Execution) -> serde_json::Value {
    let Execution::Applied(crate::tools::ToolUpdate::Finished { events, .. }) = execution else {
        panic!("expected an auto-executed tool result");
    };
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output.0),
            _ => None,
        })
        .expect("expected a ToolCallFinished event")
}

fn is_error(output: &serde_json::Value) -> bool {
    output["is_error"] == json!(true)
}

/// Unwraps a completion expected to be finished.
fn expect_finished(completion: BashCompletion) -> ToolCallResult {
    match completion {
        BashCompletion::ApprovalJudged(judgment) => {
            panic!("expected a finished bash completion, got judge result: {judgment:?}")
        }
        BashCompletion::Finished(result) => result,
        BashCompletion::DomainGrantRequired {
            call_id, domains, ..
        } => panic!(
            "expected a finished bash completion, got a host-side domain grant for \
             {call_id:?} ({domains:?})"
        ),
    }
}

#[test]
fn workspace_snapshot_tool_is_read_only_auto_allow() {
    assert_eq!(
        permission_for_tool("workspace_snapshot"),
        Some(ToolPermission::AutoAllowRead)
    );
}

#[test]
fn fs_read_glob_grep_are_auto_allow_read_and_write_edit_require_approval() {
    assert_eq!(
        permission_for_tool("fs_read"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs_glob"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs_grep"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs_write"),
        Some(ToolPermission::RequireApproval)
    );
    assert_eq!(
        permission_for_tool("fs_edit"),
        Some(ToolPermission::RequireApproval)
    );
}

#[test]
fn web_tools_enter_call_level_boundary_policy() {
    assert_eq!(
        permission_for_tool("web_search"),
        Some(ToolPermission::RequireApproval)
    );
    assert_eq!(
        permission_for_tool("web_fetch"),
        Some(ToolPermission::RequireApproval)
    );
}

#[test]
fn processing_preserves_provider_payload_on_original_event_only() {
    let tool_state = dummy_tool_state();
    let call_id = ToolCallId("call-1".to_string());
    let payload = json!({ "provider": "rig", "version": 1 });
    let processing = process_agent_provider_event(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        ProviderEvent::with_provider_payload(
            Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace_snapshot".to_string(),
                input: json!({}).into(),
                occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
            }),
            payload.clone(),
        ),
    );

    std::assert_matches!(
        &processing.horizon_events[0], ProviderEvent::Event { provider_payload: Some(value), .. } if value == &payload
    );
    assert!(
        processing
            .horizon_events
            .iter()
            .skip(1)
            .all(|event| matches!(
                event,
                ProviderEvent::Event {
                    provider_payload: None,
                    ..
                }
            ))
    );
}

// --- fs_read -----------------------------------------------------------

/// A relative path is read from the workspace root, where bash starts.
#[test]
fn fs_read_takes_a_path_relative_to_the_workspace_root() {
    let root = temp_workspace("relative-read");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/notes.txt"), "from the root\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    let output =
        sync_tools::execute_auto(&tool_state, "fs_read", &json!({ "path": "sub/notes.txt" }))
            .expect("fs_read is auto-executed");

    assert!(!is_error(&output), "{output}");
    assert!(
        output["content"]
            .as_str()
            .unwrap_or_default()
            .contains("from the root")
            || output.to_string().contains("from the root"),
        "{output}"
    );
}

/// A relative path may not climb out of the root with `..`.
#[test]
fn fs_read_rejects_a_relative_path_that_climbs_out() {
    let tool_state = dummy_tool_state();
    let output =
        sync_tools::execute_auto(&tool_state, "fs_read", &json!({ "path": "../outside.txt" }))
            .expect("fs_read is auto-executed");

    assert!(is_error(&output));
    assert!(
        output["message"].as_str().unwrap().contains(".."),
        "{output}"
    );
}

#[test]
fn fs_read_rejects_path_escaping_workspace_root() {
    let root = temp_workspace("root");
    let outside = temp_workspace("outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, "top secret").unwrap();

    let tool_state = ToolSessionState::new(root);
    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": outside_file.display().to_string() }),
    )
    .expect("fs_read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("escapes"));
}

/// An out-of-root `fs_read` routes to the approval gate, not auto-execute.
#[test]
fn fs_read_out_of_root_routes_to_approval() {
    let root = temp_workspace("ootr-routing");
    let outside = temp_workspace("ootr-outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, "top secret").unwrap();

    let tool_state = ToolSessionState::new(root);
    let execution = execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs_read".to_string(),
            input: json!({ "path": outside_file.display().to_string() }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
        },
    );

    std::assert_matches!(execution, Execution::AwaitApproval(_));
}

/// An in-root `fs_read` still auto-executes (the routing is only for
/// out-of-root paths).
#[test]
fn fs_read_in_root_auto_executes() {
    let root = temp_workspace("ootr-inroot");
    let file = root.join("file.txt");
    fs::write(&file, "hello").unwrap();

    let tool_state = ToolSessionState::new(root);
    let execution = execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs_read".to_string(),
            input: json!({ "path": file.display().to_string() }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
        },
    );

    std::assert_matches!(
        execution,
        Execution::Applied(crate::tools::ToolUpdate::Finished { .. })
    );
}

/// `execute_approved` with `allow_out_of_root = true` (the post-approval
/// path for `fs_read`) reads an out-of-root file successfully.
#[test]
fn fs_read_approved_reads_out_of_root_file() {
    let root = temp_workspace("ootr-approved");
    let outside = temp_workspace("ootr-approved-outside");
    let outside_file = outside.join("data.txt");
    fs::write(&outside_file, "external content").unwrap();

    let tool_state = ToolSessionState::new(root);
    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_read",
        &json!({ "path": outside_file.display().to_string() }),
    );

    assert!(!is_error(&output));
    assert!(
        output["content"]
            .as_str()
            .unwrap()
            .contains("external content")
    );
}

/// `fs_grep` and `fs_glob` out-of-root base paths also route to approval.
#[test]
fn fs_grep_and_glob_out_of_root_route_to_approval() {
    let root = temp_workspace("ootr-grep-glob");
    let outside = temp_workspace("ootr-grep-glob-outside");
    fs::write(outside.join("file.txt"), "x").unwrap();

    let tool_state = ToolSessionState::new(root.clone());
    let input = json!({ "base_path": outside.display().to_string(), "pattern": "x" });
    let execution = execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &ToolCallRequest {
            call_id: ToolCallId("call-grep".to_string()),
            tool_id: "fs_grep".to_string(),
            input: input.clone().into(),
            occurrence_id: crate::contract::OccurrenceId("call-grep".to_string()),
        },
    );
    std::assert_matches!(execution, Execution::AwaitApproval(_));

    let execution = execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &ToolCallRequest {
            call_id: ToolCallId("call-glob".to_string()),
            tool_id: "fs_glob".to_string(),
            input: json!({ "base_path": outside.display().to_string(), "pattern": "*.txt" }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-glob".to_string()),
        },
    );
    std::assert_matches!(execution, Execution::AwaitApproval(_));
}

// --- Git metadata reads and the unattended refusal ----------------------

/// A linked worktree laid out on disk the way `git worktree add` leaves
/// one, built with plain file writes: nothing here invokes git, so no
/// `GIT_*` variable and no global or system git config can reach the
/// fixture.
struct LinkedWorktreeLayout {
    root: PathBuf,
    main: PathBuf,
    workspace: PathBuf,
    worktree_git_dir: PathBuf,
    common_git_dir: PathBuf,
}

fn linked_worktree_layout(label: &str) -> LinkedWorktreeLayout {
    let root = temp_workspace(label);
    let main = root.join("main");
    let workspace = root.join("worktree");
    let common_git_dir = main.join(".git");
    let worktree_git_dir = common_git_dir.join("worktrees").join("agent");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(common_git_dir.join("objects")).unwrap();
    fs::create_dir_all(common_git_dir.join("refs")).unwrap();
    fs::create_dir_all(&worktree_git_dir).unwrap();
    fs::write(common_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .unwrap();
    fs::write(
        worktree_git_dir.join("gitdir"),
        workspace.join(".git").display().to_string(),
    )
    .unwrap();
    fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
    fs::write(worktree_git_dir.join("HEAD"), "ref: refs/heads/agent\n").unwrap();
    LinkedWorktreeLayout {
        root,
        main,
        workspace,
        worktree_git_dir,
        common_git_dir,
    }
}

fn read_request(call_id: &str, tool_id: &str, input: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId(call_id.to_string()),
        tool_id: tool_id.to_string(),
        input: input.into(),
        occurrence_id: crate::contract::OccurrenceId((ToolCallId(call_id.to_string())).0.clone()),
    }
}

/// A session in a linked worktree reads its own Git metadata — the gitdir
/// its `.git` file points to and the repository's common dir — without an
/// approval, the same way a session in an ordinary checkout reads the
/// `.git/` directory sitting inside its root.
#[test]
fn reads_reach_a_linked_worktrees_own_git_metadata() {
    let layout = linked_worktree_layout("metadata-reads");
    let tool_state = ToolSessionState::new(layout.workspace.clone());

    for path in [
        layout.worktree_git_dir.join("HEAD"),
        layout.common_git_dir.join("HEAD"),
    ] {
        let request = read_request(
            "call-meta-read",
            "fs_read",
            json!({ "path": path.display().to_string() }),
        );
        let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
        let output = tool_output(execution);
        assert!(
            !is_error(&output),
            "{} must be readable from its own worktree: {output}",
            path.display()
        );
        assert!(output["content"].as_str().unwrap().contains("ref: refs/"));
    }

    for (tool_id, input) in [
        (
            "fs_grep",
            json!({
                "base_path": layout.common_git_dir.display().to_string(),
                "pattern": "refs",
            }),
        ),
        (
            "fs_glob",
            json!({
                "base_path": layout.worktree_git_dir.display().to_string(),
                "pattern": "*",
            }),
        ),
    ] {
        let execution = execute_agent_tool(
            &StubHostTools,
            &tool_state,
            SessionId::new(),
            &read_request("call-meta-scan", tool_id, input),
        );
        std::assert_matches!(
            execution,
            Execution::Applied(crate::tools::ToolUpdate::Finished { .. }),
            "{tool_id} over this session's Git metadata must not need approval: {execution:?}"
        );
    }

    fs::remove_dir_all(layout.root).unwrap();
}

/// Reads reaching the Git metadata buys writes nothing: `fs_write`/`fs_edit`
/// stay confined to the workspace root, so even the post-approval execution
/// path refuses them there.
#[test]
fn writes_never_reach_the_git_metadata_a_read_may() {
    let layout = linked_worktree_layout("metadata-writes");
    let tool_state = ToolSessionState::new(layout.workspace.clone());
    let head = layout.worktree_git_dir.join("HEAD");
    let before = fs::read_to_string(&head).unwrap();

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": head.display().to_string(), "content": "ref: refs/heads/hijacked\n" }),
    );
    assert!(is_error(&output), "{output}");
    assert!(output["message"].as_str().unwrap().contains("escapes"));

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [{
                "path": head.display().to_string(),
                "old_text": "main",
                "new_text": "hijacked",
            }],
        }),
    );
    assert!(is_error(&output), "{output}");

    assert_eq!(fs::read_to_string(&head).unwrap(), before);
    fs::remove_dir_all(layout.root).unwrap();
}

/// An out-of-root read in an ordinary (attended) session is unchanged: it
/// routes to the approval gate and there is no refusal to substitute.
#[test]
fn an_attended_session_still_asks_for_an_out_of_root_read() {
    let root = temp_workspace("attended-asks");
    let outside = temp_workspace("attended-asks-outside");
    let outside_file = outside.join("notes.txt");
    fs::write(&outside_file, "elsewhere").unwrap();

    let tool_state = ToolSessionState::new(root);
    let request = read_request(
        "call-attended",
        "fs_read",
        json!({ "path": outside_file.display().to_string() }),
    );

    std::assert_matches!(
        execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request),
        Execution::AwaitApproval(_)
    );
    assert!(
        crate::tools::unattended_refusal_result(&tool_state, &request).is_none(),
        "an attended session has someone to ask"
    );
    let events = crate::tools::test_support::policy_events(
        &Event::ToolCallRequested(request),
        &tool_state,
        SessionId::new(),
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_)))
    );

    fs::remove_dir_all(outside).unwrap();
}

/// The refusal an unattended session gets names the root it may read.
#[test]
fn the_refusal_names_the_workspace_root() {
    let root = temp_workspace("refusal-root");
    let outside = temp_workspace("refusal-root-outside");
    let outside_file = outside.join("Cargo.toml");
    fs::write(&outside_file, "[package]\n").unwrap();

    let tool_state = crate::tools::ToolSessionBuilder::new(root.clone())
        .with_unattended(true)
        .build();
    let result = crate::tools::unattended_refusal_result(
        &tool_state,
        &read_request(
            "call-refused",
            "fs_read",
            json!({ "path": outside_file.display().to_string() }),
        ),
    )
    .expect("an unattended session refuses instead of asking");

    assert!(result.is_error());
    let message = result.output["message"].as_str().unwrap().to_string();
    assert!(message.contains(&root.display().to_string()), "{message}");
    assert!(
        message.contains(&outside_file.display().to_string()),
        "{message}"
    );
    assert!(message.contains("Work from paths under"), "{message}");

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

/// A path inside another checkout of the same repository is answered with
/// this session's own copy of it — the main worktree and a sibling linked
/// worktree alike.
#[test]
fn the_refusal_points_at_this_workspaces_copy_of_another_checkouts_path() {
    let layout = linked_worktree_layout("refusal-sibling");
    let sibling = layout.root.join("sibling");
    fs::create_dir_all(sibling.join("src")).unwrap();
    fs::create_dir_all(layout.common_git_dir.join("worktrees").join("sibling")).unwrap();
    fs::write(
        layout
            .common_git_dir
            .join("worktrees")
            .join("sibling")
            .join("gitdir"),
        sibling.join(".git").display().to_string(),
    )
    .unwrap();
    for tree in [&layout.main, &sibling, &layout.workspace] {
        fs::create_dir_all(tree.join("src")).unwrap();
        fs::write(tree.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
    }

    let tool_state = crate::tools::ToolSessionBuilder::new(layout.workspace.clone())
        .with_unattended(true)
        .build();
    let expected = layout
        .workspace
        .join("src")
        .join("lib.rs")
        .display()
        .to_string();

    for other in [&layout.main, &sibling] {
        let result = crate::tools::unattended_refusal_result(
            &tool_state,
            &read_request(
                "call-other-checkout",
                "fs_read",
                json!({ "path": other.join("src").join("lib.rs").display().to_string() }),
            ),
        )
        .expect("an unattended session refuses instead of asking");
        let message = result.output["message"].as_str().unwrap().to_string();
        assert!(
            message.contains("another checkout of the same repository"),
            "{message}"
        );
        assert!(message.contains(&expected), "{message}");
    }

    fs::remove_dir_all(layout.root).unwrap();
}

/// A path argument that is missing or non-string does not trigger the
/// out-of-root routing — it falls through to auto-execute, which then
/// returns an error (not a boundary crossing).
#[test]
fn fs_read_missing_path_arg_does_not_route_to_approval() {
    let root = temp_workspace("ootr-missing");
    let tool_state = ToolSessionState::new(root);
    let execution = execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs_read".to_string(),
            input: json!({}).into(),
            occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
        },
    );

    std::assert_matches!(
        execution,
        Execution::Applied(crate::tools::ToolUpdate::Finished { .. })
    );
}

#[test]
fn fs_read_windows_lines_and_reports_truncation_notice() {
    let root = temp_workspace("read-window");
    let file = root.join("lines.txt");
    let content = (1..=10)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&file, content).unwrap();

    let tool_state = ToolSessionState::new(root);
    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": file.display().to_string(), "limit": 3 }),
    )
    .expect("fs_read is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["start_line"], 1);
    assert_eq!(output["end_line"], 3);
    assert_eq!(output["total_lines"], 10);
    assert_eq!(output["truncated"], true);
    assert_eq!(output["next_offset"], 4);
    assert!(
        output["notice"]
            .as_str()
            .unwrap()
            .contains("Showing lines 1-3 of 10")
    );
    assert!(output["content"].as_str().unwrap().contains("line 1"));
    assert!(!output["content"].as_str().unwrap().contains("line 4"));
}

#[test]
fn fs_read_uses_a_smaller_default_but_allows_an_explicit_larger_window() {
    let root = temp_workspace("read-default-window");
    let file = root.join("lines.txt");
    let content = (1..=2100)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&file, content).unwrap();
    let tool_state = ToolSessionState::new(root);

    let default_output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": file.display().to_string() }),
    )
    .expect("fs_read is auto-executed");
    assert_eq!(default_output["end_line"], 500);
    assert_eq!(default_output["next_offset"], 501);

    let explicit_output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": file.display().to_string(), "limit": 2000 }),
    )
    .expect("fs_read is auto-executed");
    assert_eq!(explicit_output["end_line"], 2000);
    assert_eq!(explicit_output["next_offset"], 2001);
}

#[test]
fn fs_read_caps_total_content_at_a_line_boundary_and_returns_next_offset() {
    let root = temp_workspace("read-character-cap");
    let file = root.join("wide.txt");
    let content = (1..=100)
        .map(|line| format!("{line}:{}", "x".repeat(1000)))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&file, content).unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": file.display().to_string(), "limit": 100 }),
    )
    .expect("fs_read is auto-executed");

    let content = output["content"].as_str().unwrap();
    assert!(content.chars().count() <= 50_000);
    assert_eq!(
        output["content_chars"].as_u64().unwrap() as usize,
        content.chars().count()
    );
    assert!(output["end_line"].as_u64().unwrap() < 100);
    assert_eq!(
        output["next_offset"].as_u64().unwrap(),
        output["end_line"].as_u64().unwrap() + 1
    );
    assert!(
        output["notice"]
            .as_str()
            .unwrap()
            .contains("50000-character cap")
    );
    assert!(output["content_version"].is_string());
}

#[test]
fn execute_agent_tool_dispatches_fs_read_through_auto_execution() {
    let root = temp_workspace("dispatch-fs-read");
    let target = root.join("file.txt");
    fs::write(&target, "hello").unwrap();
    let tool_state = ToolSessionState::new(root);
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_read".to_string(),
        input: json!({ "path": target.display().to_string() }).into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let output = tool_output(execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &request,
    ));

    assert!(!is_error(&output));
    assert!(output["content"].as_str().unwrap().contains("hello"));
}

// --- unknown tool ids ------------------------------------------------------

#[test]
fn unavailable_synchronous_tools_return_identified_failure_to_the_provider() {
    struct UnavailableHost;
    impl HostTools for UnavailableHost {
        fn execute_auto(&self, _: &str, _: &serde_json::Value) -> Option<serde_json::Value> {
            None
        }
    }

    let tool_state = ToolSessionState::without_root();
    for tool_id in [
        "knowledge_read",
        "knowledge_write",
        "fs_read",
        "workspace_snapshot",
    ] {
        let request = ToolCallRequest {
            call_id: ToolCallId("unavailable".into()),
            occurrence_id: crate::contract::OccurrenceId("attempt".into()),
            tool_id: tool_id.into(),
            input: json!({ "path": "/missing/file" }).into(),
        };
        let identity = request.identity();
        let processing = process_agent_provider_event(
            &UnavailableHost,
            &tool_state,
            SessionId::new(),
            Event::ToolCallRequested(request),
        );
        let [Command::ToolCallResult(result)] = processing.provider_commands.as_slice() else {
            panic!("{tool_id} did not return exactly one tool result: {processing:?}");
        };
        assert_eq!(result.call_id, identity.call_id);
        assert_eq!(result.occurrence_id, identity.occurrence_id);
        assert!(result.is_error(), "{tool_id}: {result:?}");
        assert_eq!(
            processing
                .horizon_events
                .iter()
                .filter(|event| {
                    matches!(event.as_event(), Some(Event::ToolCallFinished(finished)) if finished == result)
                })
                .count(),
            1
        );
    }
}
//
// The 2026-07-19 dogfooding bug: the model called a nonexistent `write` tool
// (the catalog id is `fs_write`); a real `Event::ApprovalRequested` reached
// the human, and only *after* approving did the call fail with a bare
// session `Event::Error` the model never saw as a tool outcome (no matching
// `ToolCallFinished`, so the turn stalled). An unknown tool id must instead
// resolve immediately to a `ToolCallFinished` error result -- never an
// approval prompt, never a bare session error -- so the model can correct
// itself in the same turn.

#[test]
fn execute_agent_tool_reports_an_unknown_tool_as_an_error_tool_result() {
    let tool_state = dummy_tool_state();
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "write".to_string(),
        input: json!({ "path": "/tmp/x", "content": "hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let Execution::Applied(crate::tools::ToolUpdate::Finished { events, .. }) =
        execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request)
    else {
        panic!("expected Execution::Unknown for an unrecognized tool id");
    };

    let result = events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result),
            _ => None,
        })
        .expect("unknown tool id must resolve to a ToolCallFinished, not a bare Event::Error");

    assert_eq!(result.call_id, ToolCallId("call-1".to_string()));
    assert!(is_error(&result.output));
    let message = result.output["message"].as_str().unwrap();
    assert!(message.contains("Unknown tool `write`"));
    assert!(
        message.contains("fs_write"),
        "message should list available tools so the model can self-correct: {message}"
    );
}

#[test]
fn process_agent_provider_event_never_asks_approval_for_an_unknown_tool_and_continues_the_turn() {
    let tool_state = dummy_tool_state();
    let call_id = ToolCallId("call-1".to_string());
    let processing = process_agent_provider_event(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "write".to_string(),
            input: json!({}).into(),
            occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
        }),
    );

    assert!(
        !processing.horizon_events.iter().any(|event| matches!(
            event.clone().into_event().expect("conversation event"),
            Event::ApprovalRequested(_)
        )),
        "an unknown tool id must never reach a human approval prompt: {:?}",
        processing.horizon_events
    );

    let result = processing
        .horizon_events
        .iter()
        .find_map(
            |event| match &event.clone().into_event().expect("conversation event") {
                Event::ToolCallFinished(result) => Some(result.clone()),
                _ => None,
            },
        )
        .expect("expected a ToolCallFinished error result the model can see");
    assert!(is_error(&result.output));

    // The turn continues: the provider gets the result back as a real
    // `Command::ToolCallResult`, exactly like any other tool's outcome, so
    // the model can correct itself instead of the turn stalling forever.
    assert!(
        processing
            .provider_commands
            .iter()
            .any(|command| matches!(command, Command::ToolCallResult(r) if r.call_id == call_id))
    );
}

// --- fs_write ------------------------------------------------------------

#[test]
fn fs_write_creates_parent_dirs_for_new_file() {
    let root = temp_workspace("write-new");
    let target = root.join("nested").join("dir").join("file.txt");
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": target.display().to_string(), "content": "hello" }),
    );

    assert!(!is_error(&output));
    assert_eq!(output["created"], true);
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
}

#[test]
fn fs_write_overwrite_without_prior_read_is_error() {
    let root = temp_workspace("write-stale-unread");
    let target = root.join("existing.txt");
    fs::write(&target, "original").unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": target.display().to_string(), "content": "overwritten" }),
    );

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("has not been read")
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn fs_write_overwrite_after_read_succeeds() {
    let root = temp_workspace("write-after-read");
    let target = root.join("existing.txt");
    fs::write(&target, "original").unwrap();
    let tool_state = ToolSessionState::new(root);

    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": target.display().to_string(), "content": "overwritten" }),
    );

    assert!(!is_error(&output));
    assert_eq!(output["created"], false);
    assert_eq!(fs::read_to_string(&target).unwrap(), "overwritten");
}

#[test]
fn fs_write_overwrite_stale_after_external_modification_is_error() {
    let root = temp_workspace("write-stale-modified");
    let target = root.join("existing.txt");
    fs::write(&target, "original").unwrap();
    let tool_state = ToolSessionState::new(root);

    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );
    fs::write(&target, "changed underneath").unwrap();
    bump_mtime(&target);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": target.display().to_string(), "content": "overwritten" }),
    );

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("changed on disk")
    );
}

#[test]
fn fs_write_rejects_new_file_path_escaping_workspace_root() {
    let root = temp_workspace("write-escape-root");
    let outside = temp_workspace("write-escape-outside");
    let target = outside.join("new.txt");
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": target.display().to_string(), "content": "x" }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("escapes"));
    assert!(!target.exists());
}

// --- fs_edit ---------------------------------------------------------------

/// `fs_edit` takes only a list, so even a single replacement is one-element
/// `edits`.
fn one_edit(path: &Path, old_string: &str, new_string: &str) -> serde_json::Value {
    json!({
        "edits": [{
            "path": path.display().to_string(),
            "old_string": old_string,
            "new_string": new_string,
        }],
    })
}

#[test]
fn fs_edit_without_prior_read_is_error() {
    let root = temp_workspace("edit-unread");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);

    let output =
        sync_tools::execute_approved(&tool_state, "fs_edit", &one_edit(&target, "world", "there"));

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("has not been read")
    );
}

#[test]
fn fs_edit_zero_matches_is_error() {
    let root = temp_workspace("edit-zero-matches");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &one_edit(&target, "not present", "x"),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("not found"));
    assert_eq!(output["failed_index"], 0);
    assert_eq!(output["edits"][0]["status"], "failed");
}

#[test]
fn fs_edit_multiple_matches_is_error() {
    let root = temp_workspace("edit-multi-matches");
    let target = root.join("file.txt");
    fs::write(&target, "dup dup dup").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let output =
        sync_tools::execute_approved(&tool_state, "fs_edit", &one_edit(&target, "dup", "x"));

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("found 3 matches")
    );
}

#[test]
fn fs_edit_success_updates_mtime_and_allows_chained_edit() {
    let root = temp_workspace("edit-chain");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let first =
        sync_tools::execute_approved(&tool_state, "fs_edit", &one_edit(&target, "world", "there"));
    assert!(!is_error(&first));
    assert_eq!(first["edits"][0]["occurrences"], 1);
    assert_eq!(first["applied_count"], 1);
    assert_eq!(first["file_count"], 1);
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello there");

    // No re-read in between: the edit above must have refreshed the
    // recorded mtime itself, or this would fail the staleness gate.
    let second =
        sync_tools::execute_approved(&tool_state, "fs_edit", &one_edit(&target, "there", "again"));
    assert!(!is_error(&second));
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello again");
}

#[test]
fn fs_edit_stale_after_external_modification_is_error() {
    let root = temp_workspace("edit-stale");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    fs::write(&target, "hello world, modified externally").unwrap();
    bump_mtime(&target);

    let output =
        sync_tools::execute_approved(&tool_state, "fs_edit", &one_edit(&target, "world", "there"));

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("changed on disk")
    );
}

#[test]
fn fs_edit_replace_all_replaces_every_occurrence_and_reports_count() {
    let root = temp_workspace("edit-replace-all");
    let target = root.join("file.txt");
    fs::write(&target, "dup dup dup").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [{
                "path": target.display().to_string(),
                "old_string": "dup",
                "new_string": "replaced",
                "replace_all": true,
            }],
        }),
    );

    assert!(!is_error(&output));
    assert_eq!(output["edits"][0]["occurrences"], 3);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "replaced replaced replaced"
    );
}

#[test]
fn fs_edit_zero_matches_is_error_even_with_replace_all() {
    let root = temp_workspace("edit-replace-all-zero");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [{
                "path": target.display().to_string(),
                "old_string": "not present",
                "new_string": "x",
                "replace_all": true,
            }],
        }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("not found"));
}

#[test]
fn fs_edit_applies_a_batch_across_several_files_in_one_call() {
    let root = temp_workspace("edit-batch-files");
    let first = root.join("first.txt");
    let second = root.join("second.txt");
    fs::write(&first, "one\ntwo\n").unwrap();
    fs::write(&second, "before\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    for path in [&first, &second] {
        sync_tools::execute_auto(
            &tool_state,
            "fs_read",
            &json!({ "path": path.display().to_string() }),
        );
    }

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [
                { "path": first.display().to_string(), "old_string": "one", "new_string": "ONE" },
                { "path": second.display().to_string(), "old_string": "before", "new_string": "after" },
                { "path": first.display().to_string(), "old_string": "two", "new_string": "TWO" },
            ],
        }),
    );

    assert!(!is_error(&output), "{output}");
    assert_eq!(output["applied_count"], 3);
    assert_eq!(output["file_count"], 2);
    for index in 0..3 {
        assert_eq!(output["edits"][index]["status"], "applied");
        assert_eq!(output["edits"][index]["index"], index);
        assert_eq!(output["edits"][index]["occurrences"], 1);
    }
    assert_eq!(fs::read_to_string(first).unwrap(), "ONE\nTWO\n");
    assert_eq!(fs::read_to_string(second).unwrap(), "after\n");
}

#[test]
fn fs_edit_places_every_edit_against_the_file_as_it_was() {
    // The second edit names text the first one removes: each is placed
    // against the file before the call, so both apply.
    let root = temp_workspace("edit-batch-original");
    let target = root.join("file.txt");
    fs::write(&target, "hello world\nsecond line\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    read_files(&tool_state, &[&target]);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [
                { "path": target.display().to_string(), "old_string": "world", "new_string": "there" },
                { "path": target.display().to_string(), "old_string": "second", "new_string": "2nd" },
            ],
        }),
    );

    assert!(!is_error(&output), "{output}");
    assert_eq!(output["applied_count"], 2);
    assert_eq!(output["file_count"], 1);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "hello there\n2nd line\n"
    );
}

#[test]
fn fs_edit_refuses_two_edits_of_one_file_that_overlap() {
    let root = temp_workspace("edit-batch-overlap");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    read_files(&tool_state, &[&target]);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [
                { "path": target.display().to_string(), "old_string": "hello world", "new_string": "a" },
                { "path": target.display().to_string(), "old_string": "world", "new_string": "b" },
            ],
        }),
    );

    assert!(is_error(&output), "{output}");
    assert_eq!(output["failed_index"], 1);
    assert!(
        output["edits"][1]["message"]
            .as_str()
            .unwrap()
            .contains("overlaps the edit at index 0")
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
}

fn read_files(tool_state: &ToolSessionState, paths: &[&std::path::Path]) {
    for path in paths {
        sync_tools::execute_auto(
            tool_state,
            "fs_read",
            &json!({ "path": path.display().to_string() }),
        );
    }
}

#[test]
fn fs_edit_writes_nothing_when_an_edit_fails_and_names_each_failure() {
    let root = temp_workspace("edit-batch-all-or-nothing");
    let first = root.join("first.txt");
    let second = root.join("second.txt");
    let third = root.join("third.txt");
    fs::write(&first, "one\n").unwrap();
    fs::write(&second, "two\n").unwrap();
    fs::write(&third, "three\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    read_files(&tool_state, &[&first, &second, &third]);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [
                { "path": first.display().to_string(), "old_string": "one", "new_string": "ONE" },
                { "path": second.display().to_string(), "old_string": "absent", "new_string": "x" },
                { "path": third.display().to_string(), "old_string": "missing", "new_string": "y" },
            ],
        }),
    );

    assert!(is_error(&output), "{output}");
    assert_eq!(output["failed_index"], 1);
    assert_eq!(output["applied_count"], 0);
    assert_eq!(output["edits"][0]["status"], "not_attempted");
    assert_eq!(output["edits"][1]["status"], "failed");
    assert_eq!(output["edits"][2]["status"], "failed");
    let message = output["message"].as_str().unwrap();
    assert!(message.contains("edit at index 1 failed"), "{message}");
    assert!(message.contains("edit at index 2 failed"), "{message}");
    assert!(message.contains("No file was written"), "{message}");
    assert_eq!(fs::read_to_string(first).unwrap(), "one\n");
    assert_eq!(fs::read_to_string(second).unwrap(), "two\n");
    assert_eq!(fs::read_to_string(third).unwrap(), "three\n");
}

#[test]
fn fs_edit_finds_text_that_differs_only_in_trailing_space_quotes_or_dashes() {
    let root = temp_workspace("edit-normalized");
    let target = root.join("notes.md");
    fs::write(
        &target,
        "keep this   \nsay \u{201c}hello\u{201d} \u{2014} twice  \nkeep that\n",
    )
    .unwrap();
    let tool_state = ToolSessionState::new(root);
    read_files(&tool_state, &[&target]);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [{
                "path": target.display().to_string(),
                "old_string": "say \"hello\" - twice",
                "new_string": "say \"bye\" - once",
            }],
        }),
    );

    assert!(!is_error(&output), "{output}");
    assert_eq!(output["edits"][0]["normalized"], true);
    // Only the line the match covers is rewritten; the others keep their
    // bytes, trailing spaces included.
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "keep this   \nsay \"bye\" - once\nkeep that\n"
    );
}

#[test]
fn fs_edit_keeps_crlf_line_endings_and_a_byte_order_mark() {
    let root = temp_workspace("edit-crlf-bom");
    let target = root.join("windows.txt");
    fs::write(&target, "\u{feff}first\r\nsecond\r\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    read_files(&tool_state, &[&target]);

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [{
                "path": target.display().to_string(),
                "old_string": "first\nsecond",
                "new_string": "one\ntwo",
            }],
        }),
    );

    assert!(!is_error(&output), "{output}");
    assert_eq!(output["edits"][0]["normalized"], serde_json::Value::Null);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "\u{feff}one\r\ntwo\r\n"
    );
}

#[test]
fn fs_edit_rejects_a_malformed_list_before_applying_anything() {
    let root = temp_workspace("edit-batch-malformed");
    let target = root.join("file.txt");
    fs::write(&target, "one\n").unwrap();
    let tool_state = ToolSessionState::new(root);
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    );

    // The second entry has no `new_string`; the first one must not have run.
    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_edit",
        &json!({
            "edits": [
                { "path": target.display().to_string(), "old_string": "one", "new_string": "ONE" },
                { "path": target.display().to_string(), "old_string": "ONE" },
            ],
        }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("edits[1]"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "one\n");
}

#[test]
fn fs_edit_requires_a_non_empty_edits_list() {
    let root = temp_workspace("edit-batch-empty");
    let tool_state = ToolSessionState::new(root);

    let missing = sync_tools::execute_approved(&tool_state, "fs_edit", &json!({}));
    assert!(is_error(&missing));
    assert!(missing["message"].as_str().unwrap().contains("`edits`"));

    let empty = sync_tools::execute_approved(&tool_state, "fs_edit", &json!({ "edits": [] }));
    assert!(is_error(&empty));
    assert!(
        empty["message"]
            .as_str()
            .unwrap()
            .contains("at least one entry")
    );
}

// --- fs_glob / fs_grep ------------------------------------------------

#[test]
fn fs_glob_bounds_results_and_reports_total_count() {
    let root = temp_workspace("glob-bounded");
    for index in 0..5 {
        fs::write(root.join(format!("file-{index}.txt")), "content").unwrap();
    }
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt", "limit": 2 }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["returned_count"], 2);
    assert_eq!(output["total_matches"], 5);
    assert_eq!(output["truncated"], true);
    assert_eq!(output["matches"].as_array().unwrap().len(), 2);
}

#[test]
fn fs_grep_bounds_results_and_reports_total_count() {
    let root = temp_workspace("grep-bounded");
    for index in 0..5 {
        fs::write(
            root.join(format!("file-{index}.txt")),
            "some text\nTODO: fix this\nmore text",
        )
        .unwrap();
    }
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "TODO", "limit": 2 }),
    )
    .expect("fs_grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["returned_count"], 2);
    assert_eq!(output["total_matches"], 5);
    assert_eq!(output["truncated"], true);
}

#[test]
fn fs_grep_rejects_invalid_regex() {
    let root = temp_workspace("grep-invalid-regex");
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "(unclosed" }),
    )
    .expect("fs_grep is auto-executed");

    assert!(is_error(&output));
}

#[test]
fn fs_grep_searches_one_file_and_returns_each_matching_line() {
    let root = temp_workspace("grep-one-file-context");
    let file = root.join("target.txt");
    fs::write(
        &file,
        "unrelated\nbefore\nTODO: change this\nafter\nunrelated",
    )
    .unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({
            "base_path": file.display().to_string(),
            "pattern": "TODO"
        }),
    )
    .expect("fs_grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
    assert_eq!(output["matches"][0]["path"], file.display().to_string());
    assert_eq!(output["matches"][0]["line_number"], 3);
    // The matching line, without the lines around it.
    assert_eq!(output["matches"][0]["line"], "TODO: change this");
    assert!(output["matches"][0]["line_truncated"].is_null());
    assert!(output["matches"][0]["context_before"].is_null());
    assert!(output["matches"][0]["context_after"].is_null());
}

#[test]
fn fs_grep_cuts_a_long_matching_line() {
    let root = temp_workspace("grep-long-line");
    let file = root.join("minified.js");
    let long = format!("TODO{}", "x".repeat(2_000));
    fs::write(&file, &long).unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({"base_path": file.display().to_string(), "pattern": "TODO"}),
    )
    .expect("fs_grep is auto-executed");

    let line = output["matches"][0]["line"].as_str().unwrap();
    assert_eq!(line.chars().count(), crate::tools::fs::GREP_MAX_LINE_CHARS);
    assert!(long.starts_with(line));
    assert_eq!(output["matches"][0]["line_truncated"], true);
}

#[test]
fn fs_grep_caps_the_complete_tool_result() {
    let root = temp_workspace("grep-output-cap");
    let file = root.join("many-wide-matches.txt");
    // Locations are small, so the cap only bites at scale: one match per
    // line, each carrying a long absolute path.
    let content = (1..=2000)
        .map(|line| format!("TODO {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&file, content).unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({
            "base_path": file.display().to_string(),
            "pattern": "TODO",
            "limit": 2000
        }),
    )
    .expect("fs_grep is auto-executed");

    assert!(output.to_string().chars().count() <= 50_000);
    assert_eq!(output["total_matches"], 2000);
    assert!(output["returned_count"].as_u64().unwrap() < 2000);
    assert_eq!(output["truncated"], true);
    assert!(output["note"].as_str().unwrap().contains("output cap"));
}

/// Populates `root` with one visible file and one file each under `.git`,
/// `target`, and `node_modules` — the default traversal skip list.
fn populate_with_skipped_dirs(root: &Path) {
    fs::write(root.join("keep.txt"), "content").unwrap();
    for skipped_dir in [".git", "target", "node_modules"] {
        let dir = root.join(skipped_dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("hidden.txt"), "content").unwrap();
    }
}

#[test]
fn fs_glob_skips_default_ignored_directories() {
    let root = temp_workspace("glob-skip-dirs");
    populate_with_skipped_dirs(&root);
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt" }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
    assert_eq!(
        output["matches"][0].as_str().unwrap(),
        root.join("keep.txt").display().to_string()
    );
}

#[test]
fn fs_grep_skips_default_ignored_directories() {
    let root = temp_workspace("grep-skip-dirs");
    populate_with_skipped_dirs(&root);
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "content" }),
    )
    .expect("fs_grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
    assert_eq!(
        output["matches"][0]["path"].as_str().unwrap(),
        root.join("keep.txt").display().to_string()
    );
}

/// Sets `root` up as a (minimally) git-managed directory — a `.git` entry
/// is all `ignore::WalkBuilder` needs to treat it as a repository and
/// consult `.gitignore` — with a `.gitignore` that excludes `secret.log`
/// plus one file that isn't ignored.
fn populate_git_repo_with_gitignore(root: &Path) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".gitignore"), "secret.log\n").unwrap();
    fs::write(root.join("keep.txt"), "content").unwrap();
    fs::write(root.join("secret.log"), "content").unwrap();
}

#[test]
fn fs_glob_respects_gitignore_in_a_git_repository() {
    let root = temp_workspace("glob-gitignore");
    populate_git_repo_with_gitignore(&root);
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*" }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    let matches: Vec<&str> = output["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(matches.iter().any(|path| path.ends_with("keep.txt")));
    assert!(!matches.iter().any(|path| path.ends_with("secret.log")));
}

#[test]
fn fs_grep_respects_gitignore_in_a_git_repository() {
    let root = temp_workspace("grep-gitignore");
    populate_git_repo_with_gitignore(&root);
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "content" }),
    )
    .expect("fs_grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
    assert_eq!(
        output["matches"][0]["path"].as_str().unwrap(),
        root.join("keep.txt").display().to_string()
    );
}

/// Locks in `walk`'s `require_git` choice (the `ignore` crate's default):
/// a `.gitignore` file only takes effect inside an actual git repository,
/// not in an arbitrary directory that merely happens to contain one. A
/// future change relaxing this would silently start hiding matches for
/// non-git workspace roots.
#[test]
fn fs_glob_ignores_gitignore_file_outside_a_git_repository() {
    let directory = crate::test_support::non_repository_test_dir();
    let root = directory.path().to_path_buf();
    fs::write(root.join(".gitignore"), "secret.log\n").unwrap();
    fs::write(root.join("secret.log"), "content").unwrap();
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.log" }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
}

/// Locks in the decision to keep walking plain dotfiles/dotdirs (anything
/// other than `SKIPPED_DIR_NAMES`): `ignore::WalkBuilder` skips all hidden
/// entries by default, but this migration is scoped to adding `.gitignore`
/// support, not to newly hiding e.g. lint/CI config files from a search.
#[test]
fn fs_glob_still_walks_plain_dotfiles() {
    let root = temp_workspace("glob-dotfiles");
    fs::write(root.join(".eslintrc.json"), "{}").unwrap();
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*" }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    assert!(
        output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str().unwrap().ends_with(".eslintrc.json"))
    );
}

// The file-count and (grep-only) byte-count traversal caps below are
// shrunk under `cfg(test)` (see `agent::config`'s `default_fs_traversal_max_files`/
// `default_fs_grep_max_bytes`, which back `ToolSessionState::new`'s
// `AgentToolsConfig::default()`) specifically so these tests can trip them
// without creating tens of thousands of files or dozens of megabytes of
// content on disk.

#[test]
fn fs_glob_stops_at_file_count_cap_and_notes_truncation() {
    let root = temp_workspace("glob-file-cap");
    for index in 0..25 {
        fs::write(root.join(format!("file-{index}.txt")), "content").unwrap();
    }
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt" }),
    )
    .expect("fs_glob is auto-executed");

    assert!(!is_error(&output));
    let total_matches = output["total_matches"].as_u64().unwrap();
    assert!(
        total_matches < 25,
        "expected the file-count cap to stop the scan early, got {total_matches}"
    );
    assert!(
        output["note"]
            .as_str()
            .expect("note present when the scan is truncated")
            .contains("scan truncated")
    );
}

#[test]
fn fs_grep_stops_at_byte_cap_and_notes_truncation() {
    let root = temp_workspace("grep-byte-cap");
    // Ten files well under the (test-shrunk) file-count cap, but whose
    // combined content exceeds the (test-shrunk) byte cap — isolating the
    // byte cap from the file-count cap.
    for index in 0..10 {
        fs::write(root.join(format!("file-{index}.txt")), "x".repeat(200)).unwrap();
    }
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "x" }),
    )
    .expect("fs_grep is auto-executed");

    assert!(!is_error(&output));
    let total_matches = output["total_matches"].as_u64().unwrap();
    assert!(
        total_matches < 10,
        "expected the byte cap to stop the scan before reading every file, got {total_matches}"
    );
    assert!(
        output["note"]
            .as_str()
            .expect("note present when the scan is truncated")
            .contains("scan truncated")
    );
}

// --- approval wiring -----------------------------------------------------

fn show_approval(live: &LiveState, frame: &AgentFrame, call_id: &ToolCallId) -> AgentFrame {
    let request = frame.tool_call_request(call_id).unwrap();
    live.extend_events([Event::ApprovalRequested(ApprovalRequest {
        call_id: request.call_id.clone(),
        occurrence_id: request.occurrence_id.clone(),
        kind: ApprovalKind::Standard,
        reason: "test".into(),
    })])
}

fn requested_frame(call_id: &ToolCallId, tool_id: &str, input: serde_json::Value) -> AgentFrame {
    let mut frame = AgentFrame::empty();
    frame
        .items
        .push(AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: tool_id.to_string(),
            input: input.into(),
            occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
        }));
    frame
        .items
        .push(AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: call_id.clone(),
            occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
            kind: ApprovalKind::Standard,
            reason: "test".into(),
        }));
    frame
}

#[test]
fn resolve_approval_forwards_non_horizon_executed_tools() {
    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(&call_id, "mock_approval_required", json!({}));
    let session_id = SessionId::new();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );

    std::assert_matches!(
        outcome,
        ApprovalOutcome::Forward(Command::ApproveToolCall { identity: id }) if id.call_id == call_id);
}

#[test]
fn resolve_auto_approval_forwards_standard_candidate_without_a_prompt() {
    let call_id = ToolCallId("call-auto".to_string());
    let mut frame = requested_frame(&call_id, "mock_approval_required", json!({}));
    frame
        .items
        .retain(|item| !matches!(item, AgentFrameItem::ApprovalRequested(_)));
    let request = frame.tool_call_request(&call_id).expect("request").clone();
    let candidate = ApprovalCandidate {
        request,
        approval: ApprovalRequest {
            call_id: call_id.clone(),
            reason: "test".to_string(),
            kind: ApprovalKind::Standard,
            occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
        },
    };

    let outcome = resolve_auto_approval(&frame, SessionId::new(), &candidate);
    std::assert_matches!(
        outcome,
        ApprovalOutcome::Forward(Command::ApproveToolCall { identity: id }) if id.call_id == call_id);
    assert!(
        !frame
            .items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::ApprovalRequested(_)))
    );
}

#[test]
fn resolve_approval_forwards_when_no_runtime_registered() {
    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(
        &call_id,
        "fs_write",
        json!({ "path": "/tmp/does-not-matter.txt", "content": "x" }),
    );
    // A fresh session id that was never registered via `register_session_runtime`.
    let session_id = SessionId::new();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny { reason: None },
    );

    std::assert_matches!(
        outcome,
        ApprovalOutcome::Forward(Command::DenyToolCall { identity: id, .. }) if id.call_id == call_id);
}

#[test]
fn resolve_approval_executes_fs_write_on_approve() {
    let root = temp_workspace("approval-write");
    let target = root.join("new.txt");
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live = LiveState::new();
    register_session_runtime(session_id, tool_state, live.clone(), dummy_bash_results());

    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(
        &call_id,
        "fs_write",
        json!({ "path": target.display().to_string(), "content": "approved content" }),
    );

    live.extend_provider_events([Event::ToolCallRequested(
        frame.tool_call_request(&call_id).unwrap().clone(),
    )
    .into()])
        .unwrap();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Applied(ToolUpdate::Finished { result, .. }) = outcome else {
        panic!("expected fs_write to be executed by Horizon");
    };
    let frame = live.frame();

    assert_eq!(result.call_id, call_id);
    assert!(result.output["is_error"].is_null());
    assert_eq!(fs::read_to_string(&target).unwrap(), "approved content");
    assert!(frame.items.iter().any(|item| matches!(
        item,
        AgentFrameItem::ToolCallFinished(result) if result.call_id == call_id
    )));
}

#[test]
fn human_and_judge_approvals_share_execution_but_reject_stale_candidates() {
    for automatic in [false, true] {
        let root = temp_workspace("approval-dispatch");
        let target = root.join("new.txt");
        let session_id = SessionId::new();
        let live = LiveState::new();
        register_session_runtime(
            session_id,
            ToolSessionState::new(root),
            live.clone(),
            dummy_bash_results(),
        );
        let request = ToolCallRequest {
            call_id: ToolCallId("dispatch".into()),
            tool_id: "fs_write".into(),
            input: json!({"path":target, "content":"approved content"}).into(),
            occurrence_id: crate::contract::OccurrenceId::new(),
        };
        let approval = ApprovalRequest {
            call_id: request.call_id.clone(),
            reason: "write file".into(),
            kind: ApprovalKind::Standard,
            occurrence_id: request.occurrence_id.clone(),
        };
        let frame = live.extend_events(
            std::iter::once(Event::ToolCallRequested(request.clone()))
                .chain((!automatic).then(|| Event::ApprovalRequested(approval.clone()))),
        );
        let candidate = ApprovalCandidate {
            request: request.clone(),
            approval,
        };
        if automatic {
            let mut stale = candidate.clone();
            stale.request.input = json!({"path":target,"content":"different"}).into();
            std::assert_matches!(
                resolve_auto_approval(&frame, session_id, &stale),
                ApprovalOutcome::AlreadyResolved
            );
            let mut mismatched = candidate.clone();
            mismatched.approval.occurrence_id = crate::contract::OccurrenceId::new();
            std::assert_matches!(
                resolve_auto_approval(&frame, session_id, &mismatched),
                ApprovalOutcome::AlreadyResolved
            );
            assert!(!target.exists());
        }
        let outcome = if automatic {
            resolve_auto_approval(&frame, session_id, &candidate)
        } else {
            resolve_approval(
                &frame,
                session_id,
                frame
                    .tool_call_request(&request.call_id.clone())
                    .unwrap()
                    .identity(),
                ApprovalDecision::Approve,
            )
        };
        let ApprovalOutcome::Applied(ToolUpdate::Finished { events, result }) = outcome else {
            panic!("synchronous execution")
        };
        let frame = live.frame();

        std::assert_matches!(
            events.as_slice(),
            [
                Event::StateChanged(crate::contract::SessionState::ToolRunning),
                Event::ToolCallStarted(_),
                Event::ToolCallFinished(_),
                Event::StateChanged(crate::contract::SessionState::Running)
            ]
        );
        assert_eq!(result.occurrence_id, request.occurrence_id);
        assert!(!result.is_error());
        assert_eq!(fs::read_to_string(&target).unwrap(), "approved content");
        fs::write(&target, "later edit").unwrap();
        let repeated = if automatic {
            resolve_auto_approval(&frame, session_id, &candidate)
        } else {
            resolve_approval(
                &frame,
                session_id,
                frame
                    .tool_call_request(&request.call_id)
                    .unwrap()
                    .identity(),
                ApprovalDecision::Approve,
            )
        };
        std::assert_matches!(repeated, ApprovalOutcome::AlreadyResolved);
        assert_eq!(fs::read_to_string(target).unwrap(), "later edit");
        unregister_session_runtime(session_id);
    }
}

#[test]
fn resolve_approval_denies_fs_edit_without_running_it() {
    let root = temp_workspace("approval-deny-edit");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live = LiveState::new();
    register_session_runtime(session_id, tool_state, live.clone(), dummy_bash_results());

    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(&call_id, "fs_edit", one_edit(&target, "world", "there"));

    live.extend_provider_events([Event::ToolCallRequested(
        frame.tool_call_request(&call_id).unwrap().clone(),
    )
    .into()])
        .unwrap();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny { reason: None },
    );

    let ApprovalOutcome::Applied(ToolUpdate::Finished { result, .. }) = outcome else {
        panic!("expected fs_edit denial to be resolved by Horizon");
    };
    assert_eq!(result.output["is_error"], true);
    assert_eq!(result.output["message"], "denied by user");
    assert!(
        result.is_denied(),
        "the contract-explicit denial marker must be set"
    );
    // Never ran: the file is untouched.
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
}

#[test]
fn a_denial_with_a_reason_tells_the_model_the_reason() {
    let root = temp_workspace("approval-deny-reason");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let session_id = SessionId::new();
    let live = LiveState::new();
    register_session_runtime(
        session_id,
        ToolSessionState::new(root),
        live.clone(),
        dummy_bash_results(),
    );
    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(&call_id, "fs_edit", one_edit(&target, "world", "there"));
    live.extend_provider_events([Event::ToolCallRequested(
        frame.tool_call_request(&call_id).unwrap().clone(),
    )
    .into()])
        .unwrap();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny {
            reason: Some("  edit the copy under docs/ instead ".into()),
        },
    );

    let ApprovalOutcome::Applied(ToolUpdate::Finished { result, .. }) = outcome else {
        panic!("expected fs_edit denial to be resolved by Horizon");
    };
    assert_eq!(
        result.output["message"],
        "denied by user: edit the copy under docs/ instead"
    );
    assert!(result.is_denied());
    unregister_session_runtime(session_id);
}

#[test]
fn resolve_approval_approve_that_fails_on_its_own_does_not_set_the_denied_marker() {
    // Distinguishes a genuine denial from an *approved* call that fails for
    // its own reasons (fs_edit's "old_string not found" here): both are
    // `is_error: true`, but only a denial sets `ToolCallResult::denied`.
    let root = temp_workspace("approval-approve-then-fail");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live = LiveState::new();
    register_session_runtime(session_id, tool_state, live.clone(), dummy_bash_results());

    let call_id = ToolCallId("call-1".to_string());
    // No prior `fs_read` for this path -- `fs_edit` requires one, so this
    // approve runs the tool (it's not a denial) but the tool itself fails.
    let frame = requested_frame(&call_id, "fs_edit", one_edit(&target, "world", "there"));

    live.extend_provider_events([Event::ToolCallRequested(
        frame.tool_call_request(&call_id).unwrap().clone(),
    )
    .into()])
        .unwrap();

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Applied(ToolUpdate::Finished { result, .. }) = outcome else {
        panic!("expected fs_edit approve to resolve synchronously");
    };
    assert_eq!(result.output["is_error"], true);
    assert!(
        !result.is_denied(),
        "an approve that fails on its own is not a denial"
    );
    // Never mutated: the edit itself failed.
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
}

#[test]
fn resolve_approval_second_approve_is_noop() {
    let root = temp_workspace("approval-double-approve");
    let target = root.join("file.txt");
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    // Fold the request through the session's LiveState, as production does:
    // the frame the UI hands to `resolve_approval` and the frame the
    // execution updates are the same accumulated frame.
    let call_id = ToolCallId("call-1".to_string());
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "first" }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);

    let frame = show_approval(&live_state, &frame, &call_id);
    let first = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );
    let ApprovalOutcome::Applied(ToolUpdate::Finished { .. }) = first else {
        panic!("first approve should execute fs_write");
    };
    let updated_frame = live_state.frame();
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");

    // Prove the duplicate doesn't re-run the tool: change the file on disk;
    // a re-executed write would clobber (or error on) this content.
    fs::write(&target, "externally changed").unwrap();

    let second = resolve_approval(
        &updated_frame,
        session_id,
        updated_frame
            .tool_call_request(&call_id)
            .unwrap()
            .identity(),
        ApprovalDecision::Approve,
    );
    std::assert_matches!(second, ApprovalOutcome::AlreadyResolved);
    assert_eq!(fs::read_to_string(&target).unwrap(), "externally changed");
}

#[test]
fn resolve_approval_deny_then_approve_is_noop() {
    let root = temp_workspace("approval-deny-then-approve");
    let target = root.join("file.txt");
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("call-1".to_string());
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "should never land" })
            .into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);

    let frame = show_approval(&live_state, &frame, &call_id);
    let denied = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny { reason: None },
    );
    let ApprovalOutcome::Applied(ToolUpdate::Finished { .. }) = denied else {
        panic!("deny of fs_write should be resolved by Horizon");
    };
    let updated_frame = live_state.frame();
    assert!(!target.exists());

    let approved_late = resolve_approval(
        &updated_frame,
        session_id,
        updated_frame
            .tool_call_request(&call_id)
            .unwrap()
            .identity(),
        ApprovalDecision::Approve,
    );
    std::assert_matches!(approved_late, ApprovalOutcome::AlreadyResolved);
    assert!(!target.exists());
}

#[test]
fn resolve_approval_executes_a_new_occurrence_of_a_reused_call_id() {
    // Root-caused 2026-07-18 (a real owner session, rig/OpenAI-compatible
    // provider): the provider reused the exact same call_id string for two
    // structurally different `fs_write`/`fs_edit` calls in one session --
    // the first fully resolved (approved and finished) well before the
    // model ever requested the second. The old `has_tool_call_finished`/
    // `has_tool_call_started` whole-session scan mistook the *first*
    // occurrence's finish for the *second* occurrence's, so `try_execute`
    // permanently returned `AlreadyResolved` for a call nobody had ever
    // acted on -- no tool ran, no result reached the provider, the turn
    // wedged forever with no way to recover (the host-side half of the
    // "session stuck, no working Approve" report).
    let root = temp_workspace("approval-reused-call-id");
    let target_a = root.join("a.txt");
    let target_b = root.join("b.txt");
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("dup".to_string());

    // First occurrence: requested, approved, finished -- a complete,
    // unrelated cycle that closes before the id is ever reused.
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target_a.display().to_string(), "content": "first" }).into(),
        occurrence_id: crate::contract::OccurrenceId::new(),
    })]);
    let frame = show_approval(&live_state, &frame, &call_id);
    let first = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );
    std::assert_matches!(
        first,
        ApprovalOutcome::Applied(ToolUpdate::Finished { .. }),
        "first occurrence's approve should execute fs_write"
    );
    assert_eq!(fs::read_to_string(&target_a).unwrap(), "first");

    // Second occurrence: the provider reuses `call_id` for a genuinely
    // different, later call.
    let after_second_request =
        live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "fs_write".to_string(),
            input: json!({ "path": target_b.display().to_string(), "content": "second" }).into(),
            occurrence_id: crate::contract::OccurrenceId::new(),
        })]);

    let after_second_request = show_approval(&live_state, &after_second_request, &call_id);
    let second = resolve_approval(
        &after_second_request,
        session_id,
        after_second_request
            .tool_call_request(&call_id)
            .unwrap()
            .identity(),
        ApprovalDecision::Approve,
    );
    std::assert_matches!(
        second,
        ApprovalOutcome::Applied(ToolUpdate::Finished { .. }),
        "second occurrence's approve must execute, not be swallowed as \
         AlreadyResolved just because an earlier occurrence of the same \
         call_id already finished: {second:?}"
    );
    assert_eq!(fs::read_to_string(&target_b).unwrap(), "second");
}

// --- policy tiers: tier-1 auto-approval (docs/agent-approval-design.md) ---

#[test]
fn fs_write_auto_executes_in_an_isolated_session_with_the_audit_marker() {
    let root = temp_workspace("tier1-fs-write-isolated");
    let tool_state = crate::tools::ToolSessionBuilder::new(root.clone()).build();
    let target = root.join("new.txt");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    let Execution::Applied(crate::tools::ToolUpdate::Finished { events, .. }) = execution else {
        panic!("expected a synchronous applied update for a contained fs_write");
    };
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted(id) if id == &request.identity()))
    );
    let output = events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output.0),
            _ => None,
        })
        .expect("expected a ToolCallFinished event");

    assert_eq!(fs::read_to_string(&target).unwrap(), "hi");
    assert_eq!(output["auto_approved"], true);
    assert_eq!(output["policy_tier"], "contained");
}

#[test]
fn fs_edit_auto_executes_in_an_isolated_session() {
    let root = temp_workspace("tier1-fs-edit-isolated");
    let target = root.join("file.txt");
    fs::write(&target, "before").unwrap();
    let tool_state = crate::tools::ToolSessionBuilder::new(root).build();
    // fs_edit's staleness gate needs a prior fs_read recorded -- mirrors
    // `fs_edit_success_updates_mtime_and_allows_chained_edit` above.
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    )
    .expect("fs_read is auto-executed");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_edit".to_string(),
        input: one_edit(&target, "before", "after").into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    std::assert_matches!(
        execution,
        Execution::Applied(crate::tools::ToolUpdate::Finished { .. }),
        "expected a synchronous applied update for a contained fs_edit, got {execution:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "after");
}

#[test]
fn fs_write_auto_executes_in_a_non_isolated_session() {
    let root = temp_workspace("tier1-fs-write-not-isolated");
    let tool_state = ToolSessionState::new(root.clone()); // isolation defaults to false
    let target = root.join("new.txt");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let output = tool_output(execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &request,
    ));
    assert!(!is_error(&output), "{output:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "hi");
    assert_eq!(output["auto_approved"], true);
    assert_eq!(output["policy_tier"], "contained");
}

#[test]
fn fs_edit_auto_executes_in_a_non_isolated_session() {
    let root = temp_workspace("tier1-fs-edit-not-isolated");
    let target = root.join("file.txt");
    fs::write(&target, "before").unwrap();
    let tool_state = ToolSessionState::new(root); // isolation defaults to false
    sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": target.display().to_string() }),
    )
    .expect("fs_read is auto-executed");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_edit".to_string(),
        input: one_edit(&target, "before", "after").into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let output = tool_output(execute_agent_tool(
        &StubHostTools,
        &tool_state,
        SessionId::new(),
        &request,
    ));
    assert!(!is_error(&output), "{output:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "after");
}

#[test]
fn fs_write_into_the_gitdir_waits_for_approval_and_writes_nothing() {
    let root = temp_workspace("fs-write-gitdir");
    fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    let tool_state = ToolSessionState::new(root.clone());
    let target = root.join(".git").join("hooks").join("pre-commit");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs_write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "#!/bin/sh" }).into(),
        occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    std::assert_matches!(execution, Execution::AwaitApproval(_));
    assert!(!target.exists(), "must not have run yet");
}

#[test]
fn policy_plan_omits_the_approval_prompt_for_a_contained_fs_write() {
    let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir()).build();
    let events = crate::tools::test_support::policy_events(
        &Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs_write".to_string(),
            input: json!({ "path": "/tmp/x", "content": "hi" }).into(),
            occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
        }),
        &tool_state,
        SessionId::new(),
    );
    assert_eq!(events.len(), 1, "no approval prompt expected: {events:?}");
}

/// A `bash` call in an isolated session, with the sandbox runtime ready,
/// auto-executes contained on the background thread, and its result carries
/// both audit markers (`sandboxed`, `auto_approved`/tier/reason).
#[test]
fn bash_auto_executes_sandboxed_in_an_isolated_session_with_an_engaged_sandbox() {
    let root = temp_workspace("tier1-bash-sandboxed");
    let tool_state = crate::tools::ToolSessionBuilder::new(root).build();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId::new(),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    let Execution::Applied(crate::tools::ToolUpdate::Started { events }) = execution else {
        panic!("expected an applied start for a contained bash call");
    };
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted(id) if id == &request.identity()))
    );

    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the sandboxed bash call should finish");
    let result = expect_finished(completion);
    assert_eq!(result.call_id, request.call_id);
    assert_eq!(result.occurrence_id, request.occurrence_id);
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "hi\n");
    assert_eq!(result.output["sandboxed"], true, "{:?}", result.output);
    assert_eq!(result.output["auto_approved"], true);
    assert_eq!(result.output["policy_tier"], "contained");

    unregister_session_runtime(session_id);
}

/// A session spawned in a shared directory (not an isolated worktree)
/// runs `bash` sandboxed and without approval, the same as an isolated one
/// (`docs/sandbox-perimeter-design.md`).
#[test]
fn bash_auto_executes_sandboxed_in_a_non_isolated_session() {
    let root = temp_workspace("tier1-bash-not-isolated");
    let tool_state = ToolSessionState::new(root); // isolation defaults to false
    let session_id = SessionId::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        LiveState::new(),
        bash_results_tx,
    );
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId::new(),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    std::assert_matches!(
        execution,
        Execution::Applied(crate::tools::ToolUpdate::Started { .. }),
        "expected an automatic start, got {execution:?}"
    );
    let result = expect_finished(
        bash_results_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the sandboxed bash call should finish"),
    );
    assert_eq!(result.output["output"], "hi\n");
    assert_eq!(result.output["sandboxed"], true, "{:?}", result.output);
    assert_eq!(result.output["auto_approved"], true);

    unregister_session_runtime(session_id);
}

// --- approval wiring: filesystem-denial retry ----------------------------

// --- approval wiring: domain-denial retry (leg 4b) -------------------------

#[test]
fn a_rebuilt_tool_state_keeps_the_domains_approved_before_it() {
    let previous = ToolSessionState::new(std::env::temp_dir());
    previous.allow_domain("index.crates.io");
    previous.allow_domain("github.com");
    let replacement = ToolSessionState::new(std::env::temp_dir());
    assert!(!replacement.is_domain_allowed("index.crates.io"));
    replacement.retain_domains_from(&previous);
    assert!(replacement.is_domain_allowed("index.crates.io"));
    assert!(replacement.is_domain_allowed("github.com"));
    // The replacement's own store, not the previous one: a later approval
    // lands only where the session now lives.
    replacement.allow_domain("static.crates.io");
    assert!(!previous.is_domain_allowed("static.crates.io"));
}

fn domain_grant_frame(live_state: &LiveState, call_id: &ToolCallId, domain: &str) -> AgentFrame {
    live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "web_fetch".to_string(),
        input: json!({ "url": format!("https://{domain}/") }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);
    live_state.extend_events([Event::ApprovalRequested(crate::contract::ApprovalRequest {
        call_id: call_id.clone(),
        reason: format!("allow {domain} for this session?"),
        kind: crate::contract::ApprovalKind::DomainGrant {
            domains: vec![domain.to_string()],
        },
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })])
}

#[test]
fn resolve_approval_web_fetch_deny_does_not_mutate_the_domain_policy() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        live_state.clone(),
        dummy_bash_results(),
    );
    let call_id = ToolCallId("web-fetch-domain-deny".to_string());
    let frame = domain_grant_frame(&live_state, &call_id, "example.com");

    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny { reason: None },
    );

    std::assert_matches!(
        outcome,
        ApprovalOutcome::Applied(ToolUpdate::Finished { .. })
    );
    assert!(!tool_state.is_domain_allowed("example.com"));
    unregister_session_runtime(session_id);
}

#[test]
fn resolve_approval_web_fetch_approve_adds_only_the_exact_session_grant() {
    let approved_state = dummy_tool_state();
    let other_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        approved_state.clone(),
        live_state.clone(),
        dummy_bash_results(),
    );
    let call_id = ToolCallId("web-fetch-domain-approve".to_string());
    let request = ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "web_fetch".to_string(),
        input: json!({ "url": "https://example.com/" }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    };
    let frame = live_state.extend_events([Event::ToolCallRequested(request.clone())]);
    let approval = ApprovalRequest {
        call_id: call_id.clone(),
        reason: "allow example.com for this session?".to_string(),
        kind: ApprovalKind::DomainGrant {
            domains: vec!["example.com".to_string()],
        },
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    };
    let outcome =
        resolve_auto_approval(&frame, session_id, &ApprovalCandidate { request, approval });

    std::assert_matches!(
        outcome,
        ApprovalOutcome::Applied(ToolUpdate::Started { .. })
    );
    assert!(approved_state.is_domain_allowed("example.com"));
    assert!(!approved_state.is_domain_allowed("www.example.com"));
    assert!(!other_state.is_domain_allowed("example.com"));
    crate::tools::cancel_tool_execution(session_id, &crate::test_support::tool_identity(&call_id));
    unregister_session_runtime(session_id);
}

// --- approval wiring: bash -------------------------------------------------

#[test]
fn bash_tool_requires_approval() {
    assert_eq!(
        permission_for_tool("bash"),
        Some(ToolPermission::RequireApproval)
    );
}

#[test]
fn resolve_approval_starts_bash_on_approve_and_delivers_its_result() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state, live_state.clone(), bash_results_tx);

    let call_id = ToolCallId("bash-1".to_string());
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);

    let frame = show_approval(&live_state, &frame, &call_id);
    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Applied(ToolUpdate::Started { .. }) = outcome else {
        panic!("approving bash should start it, not finish it synchronously");
    };
    let frame = live_state.frame();
    assert!(
        frame.items.iter().any(
            |item| matches!(item, AgentFrameItem::ToolCallStarted(id) if id.call_id == call_id)
        )
    );
    assert!(!frame.has_tool_call_finished(&call_id));

    // The command actually ran on a background thread and its result
    // arrives on the channel `resolve_approval` was given.
    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the approved bash call should finish and report a result");
    let result = expect_finished(completion);
    assert_eq!(result.call_id, call_id);
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "hi\n");
    assert_eq!(result.output["sandboxed"], false);
    assert_eq!(result.output["host_execution_approved"], true);
    assert_eq!(result.output["approval_scope"], "host_execution_once");
    assert_eq!(result.output["approval_source"], "human");
}

#[test]
fn resolve_approval_denies_bash_without_running_it() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state, live_state.clone(), bash_results_tx);

    let call_id = ToolCallId("bash-2".to_string());
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo should-never-run" }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);

    let frame = show_approval(&live_state, &frame, &call_id);
    let outcome = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Deny { reason: None },
    );

    let ApprovalOutcome::Applied(ToolUpdate::Finished { result, .. }) = outcome else {
        panic!("denying bash should resolve synchronously, like fs_write/fs_edit");
    };
    assert_eq!(result.output["is_error"], true);
    assert_eq!(result.output["message"], "denied by user");
    assert!(
        result.is_denied(),
        "the contract-explicit denial marker must be set"
    );

    // Never spawned: nothing ever arrives on the results channel.
    assert!(
        bash_results_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err()
    );
}

/// Regression test for the 2026-07 repeated-approval OOM incident's root
/// cause: `resolve_approval_second_approve_is_noop` (above) proves the
/// `AlreadyResolved` guard for `fs_write`, whose `ToolCallStarted` and
/// `ToolCallFinished` are always folded together -- but `bash`'s approve
/// only folds `ToolCallStarted` immediately, leaving `ToolCallFinished` to
/// arrive whenever the child actually exits. A duplicate Approve arriving
/// in that window (a banner re-sending `Approve` on every repeated `y`
/// keypress) must still be dropped, not spawn a second concurrent process
/// for the same call. Proven by counting completions: exactly one must ever
/// arrive on the results channel, even though `resolve_approval` was called
/// twice with `Approve`.
#[test]
fn resolve_approval_second_approve_of_a_still_running_bash_call_is_noop() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state, live_state.clone(), bash_results_tx);

    let call_id = ToolCallId("bash-double-approve".to_string());
    let frame = live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo first" }).into(),
        occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
    })]);

    let frame = show_approval(&live_state, &frame, &call_id);
    let first = resolve_approval(
        &frame,
        session_id,
        frame.tool_call_request(&call_id).unwrap().identity(),
        ApprovalDecision::Approve,
    );
    let ApprovalOutcome::Applied(ToolUpdate::Started { .. }) = first else {
        panic!("first approve should start bash");
    };
    let running_frame = live_state.frame();
    // The frame the second approve would actually see (production folds
    // this exact frame, e.g. `horizon-agent-host`'s `resolve_and_forward` reads
    // `live_state.frame()` fresh for every inbound command) already shows
    // `ToolCallStarted` but not yet `ToolCallFinished` -- the vulnerable
    // window.
    assert!(running_frame.has_tool_call_started(&call_id));
    assert!(!running_frame.has_tool_call_finished(&call_id));

    let second = resolve_approval(
        &running_frame,
        session_id,
        running_frame
            .tool_call_request(&call_id)
            .unwrap()
            .identity(),
        ApprovalDecision::Approve,
    );
    std::assert_matches!(
        second,
        ApprovalOutcome::AlreadyResolved,
        "a duplicate approve of a still-running bash call must be dropped, got: {second:?}"
    );

    // Exactly one completion ever arrives -- the duplicate approve never
    // spawned its own bash child.
    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the single approved bash call should finish");
    assert_eq!(expect_finished(completion).call_id, call_id);
    assert!(
        bash_results_rx.try_recv().is_err(),
        "a duplicate approve must not have spawned a second bash process"
    );
}

#[test]
fn bash_starts_in_the_workspace_root() {
    let root = temp_workspace("bash-start-dir");
    let tool_state = ToolSessionState::new(root.clone());

    assert_eq!(tool_state.bash_dir(), root);
}

// --- path safety regressions ---------------------------------------------

#[test]
fn fs_read_rejects_parent_dir_component() {
    let root = temp_workspace("read-parent-dir");
    fs::write(root.join("file.txt"), "hello").unwrap();
    let tool_state = ToolSessionState::new(root.clone());
    // Even though this would lexically resolve back inside the root, `..`
    // is rejected outright: the confinement check is lexical, so `..` in
    // paths can't be trusted in general.
    let requested = format!("{}/sub/../file.txt", root.display());

    let output = sync_tools::execute_auto(&tool_state, "fs_read", &json!({ "path": requested }))
        .expect("fs_read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("`..`"));
}

#[test]
fn fs_read_normalizes_cur_dir_components() {
    let root = temp_workspace("read-cur-dir");
    fs::write(root.join("file.txt"), "hello").unwrap();
    let tool_state = ToolSessionState::new(root.clone());
    let requested = format!("{}/./file.txt", root.display());

    let output = sync_tools::execute_auto(&tool_state, "fs_read", &json!({ "path": requested }))
        .expect("fs_read is auto-executed");

    assert!(!is_error(&output));
    assert!(output["content"].as_str().unwrap().contains("hello"));
}

#[test]
fn fs_write_rejects_parent_dir_traversal_through_missing_dir() {
    let root = temp_workspace("write-traversal");
    let outside = temp_workspace("write-traversal-outside");
    // `{root}/missing/..(..)/{outside}/escaped.txt`: the ancestor walk stops
    // at an existing dir while `..` survives lexically, but the OS would
    // resolve the full path into `outside` — must be rejected up front.
    let outside_name = outside.file_name().unwrap().to_str().unwrap();
    let requested = format!(
        "{}/missing/../../{outside_name}/escaped.txt",
        root.display()
    );
    let tool_state = ToolSessionState::new(root.clone());

    let output = sync_tools::execute_approved(
        &tool_state,
        "fs_write",
        &json!({ "path": requested, "content": "escaped" }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("`..`"));
    assert!(!outside.join("escaped.txt").exists());
    assert!(!root.join("missing").exists());
}

#[test]
fn fs_tools_reject_all_paths_when_workspace_root_unavailable() {
    let root = temp_workspace("no-root");
    let file = root.join("file.txt");
    fs::write(&file, "hello").unwrap();
    let tool_state = ToolSessionState::without_root();

    let output = sync_tools::execute_auto(
        &tool_state,
        "fs_read",
        &json!({ "path": file.display().to_string() }),
    )
    .expect("fs_read is auto-executed");

    assert!(is_error(&output));
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("workspace root")
    );
}

// --- session runtime registry ----------------------------------------------

#[test]
fn unregister_session_runtime_removes_registered_runtime() {
    let session_id = SessionId::new();
    register_session_runtime(
        session_id,
        dummy_tool_state(),
        LiveState::new(),
        dummy_bash_results(),
    );
    assert!(session_runtime(session_id).is_some());

    unregister_session_runtime(session_id);
    assert!(session_runtime(session_id).is_none());

    // Unregistering an unknown id is a safe no-op (terminal sessions never
    // register in the first place).
    unregister_session_runtime(SessionId::new());
}

#[test]
fn ephemeral_feedback_passes_processing_without_placeholder_events() {
    let tools = ToolSessionState::without_root();
    for feedback in [
        ProviderEvent::session_model("resolved-model".into()),
        ProviderEvent::session_selection("provider".into(), "selected-model".into()),
        ProviderEvent::task_progress(crate::contract::TaskProgress {
            task_session_id: SessionId::new(),
            description: "inspect".into(),
            state: crate::contract::TaskProgressState::Running,
        }),
    ] {
        let processing = process_agent_provider_event(
            &StubHostTools,
            &tools,
            SessionId::new(),
            feedback.clone(),
        );
        assert_eq!(processing.horizon_events, [feedback]);
        assert!(processing.provider_commands.is_empty());
    }
}

/// The history of one successful call of `tool` with `input`, run against
/// `tool_state` now.
fn recorded_call(
    tool_state: &ToolSessionState,
    tool: &str,
    input: serde_json::Value,
) -> Vec<Event> {
    let output = if tool == "fs_read" {
        sync_tools::execute_auto(tool_state, tool, &input).expect("auto-executed")
    } else {
        sync_tools::execute_approved(tool_state, tool, &input)
    };
    assert!(!is_error(&output), "{output}");
    let request = ToolCallRequest {
        call_id: ToolCallId(tool.into()),
        occurrence_id: crate::contract::OccurrenceId::new(),
        tool_id: tool.into(),
        input: input.into(),
    };
    let result = ToolCallResult::new(
        request.call_id.clone(),
        request.occurrence_id.clone(),
        output,
    );
    vec![
        Event::ToolCallRequested(request),
        Event::ToolCallFinished(result),
    ]
}

#[test]
fn a_session_started_again_over_its_history_edits_what_it_read_before() {
    let root = temp_workspace("restore-file-versions");
    let read = root.join("read.txt");
    let edited = root.join("edited.txt");
    let changed = root.join("changed.txt");
    for file in [&read, &edited, &changed] {
        fs::write(file, "before\n").unwrap();
    }
    let before = ToolSessionState::new(root.clone());
    let mut history = Vec::new();
    for file in [&read, &edited, &changed] {
        history.extend(recorded_call(
            &before,
            "fs_read",
            json!({"path": file.display().to_string()}),
        ));
    }
    history.extend(recorded_call(
        &before,
        "fs_edit",
        json!({"edits": [{"path": edited.display().to_string(), "old_string": "before", "new_string": "middle"}]}),
    ));
    // Something else changes one file while the session is away.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&changed, "outside\n").unwrap();

    let after = ToolSessionState::new(root);
    restore_file_versions(&after, &history);
    let edit = |file: &PathBuf, old: &str| {
        sync_tools::execute_approved(
            &after,
            "fs_edit",
            &json!({"edits": [{"path": file.display().to_string(), "old_string": old, "new_string": "after"}]}),
        )
    };
    assert!(
        !is_error(&edit(&read, "before")),
        "a file read before is edited without a read"
    );
    assert!(
        !is_error(&edit(&edited, "middle")),
        "so is one edited before"
    );
    let refused = edit(&changed, "outside");
    assert!(is_error(&refused));
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("changed on disk"),
        "{refused}"
    );
}
