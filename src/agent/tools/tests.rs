use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::json;

use super::*;
use crate::agent::contract::{
    Command, Event, ProviderEvent, ToolCallId, ToolCallRequest, ToolPermission,
};
use crate::agent::frame::{AgentFrame, AgentFrameItem};
use crate::agent::live::LiveState;
use crate::agent::tools::execution::{execute_agent_tool, workspace_snapshot};
use crate::agent::tools::fs as fs_tools;
use crate::agent::tools::state::{
    register_session_runtime, session_runtime, unregister_session_runtime, ToolSessionState,
};
use crate::session::SessionId;
use crate::workspace::Workspace;

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
/// receiver — see `app/runtime/agent.rs`).
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
    let Execution::Auto(events) = execution else {
        panic!("expected an auto-executed tool result");
    };
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output),
            _ => None,
        })
        .expect("expected a ToolCallFinished event")
}

fn is_error(output: &serde_json::Value) -> bool {
    output["is_error"] == json!(true)
}

#[test]
fn workspace_snapshot_tool_is_read_only_auto_allow() {
    assert_eq!(
        permission_for_tool("workspace.snapshot"),
        Some(ToolPermission::AutoAllowRead)
    );
}

#[test]
fn fs_read_glob_grep_are_auto_allow_read_and_write_edit_require_approval() {
    assert_eq!(
        permission_for_tool("fs.read"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs.glob"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs.grep"),
        Some(ToolPermission::AutoAllowRead)
    );
    assert_eq!(
        permission_for_tool("fs.write"),
        Some(ToolPermission::RequireApproval)
    );
    assert_eq!(
        permission_for_tool("fs.edit"),
        Some(ToolPermission::RequireApproval)
    );
}

#[test]
fn workspace_snapshot_includes_core_workspace_state() {
    let workspace = Workspace::mvp();
    let snapshot = workspace_snapshot(&workspace);

    assert_eq!(snapshot["tab_count"], 1);
    assert_eq!(snapshot["active_title"], "Terminal #1");
    assert_eq!(snapshot["tabs"][0]["title"], "Terminal #1");
}

#[test]
fn execute_workspace_snapshot_returns_tool_result_events() {
    let workspace = Workspace::mvp();
    let tool_state = dummy_tool_state();
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "workspace.snapshot".to_string(),
        input: json!({}),
    };

    let Execution::Auto(events) = execute_agent_tool(&workspace, &tool_state, &request) else {
        panic!("workspace snapshot should auto execute");
    };

    assert!(events.iter().any(|event| {
        matches!(
            event,
            Event::ToolCallFinished(result) if result.call_id == request.call_id
        )
    }));
}

#[test]
fn processing_workspace_snapshot_returns_result_command_to_provider() {
    let workspace = Workspace::mvp();
    let tool_state = dummy_tool_state();
    let call_id = ToolCallId("call-1".to_string());
    let processing = process_agent_provider_event(
        &workspace,
        &tool_state,
        Event::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "workspace.snapshot".to_string(),
            input: json!({}),
        }),
    );

    assert!(processing.horizon_events.iter().any(|provider_event| {
        matches!(
            &provider_event.event,
            Event::ToolCallFinished(result) if result.call_id == call_id
        )
    }));
    assert!(processing.provider_commands.iter().any(|command| {
        matches!(
            command,
            Command::ToolCallResult(result) if result.call_id == call_id
        )
    }));
}

#[test]
fn processing_preserves_provider_payload_on_original_event_only() {
    let workspace = Workspace::mvp();
    let tool_state = dummy_tool_state();
    let call_id = ToolCallId("call-1".to_string());
    let payload = json!({ "provider": "rig", "version": 1 });
    let processing = process_agent_provider_event(
        &workspace,
        &tool_state,
        ProviderEvent::with_provider_payload(
            Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace.snapshot".to_string(),
                input: json!({}),
            }),
            payload.clone(),
        ),
    );

    assert_eq!(processing.horizon_events[0].provider_payload, Some(payload));
    assert!(processing
        .horizon_events
        .iter()
        .skip(1)
        .all(|event| { event.provider_payload.is_none() }));
}

// --- fs.read -----------------------------------------------------------

#[test]
fn fs_read_rejects_relative_path() {
    let tool_state = dummy_tool_state();
    let output = fs_tools::execute_auto(&tool_state, "fs.read", &json!({ "path": "relative.txt" }))
        .expect("fs.read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("absolute"));
}

#[test]
fn fs_read_rejects_path_escaping_workspace_root() {
    let root = temp_workspace("root");
    let outside = temp_workspace("outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, "top secret").unwrap();

    let tool_state = ToolSessionState::new(root);
    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": outside_file.display().to_string() }),
    )
    .expect("fs.read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("escapes"));
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
    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": file.display().to_string(), "limit": 3 }),
    )
    .expect("fs.read is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["start_line"], 1);
    assert_eq!(output["end_line"], 3);
    assert_eq!(output["total_lines"], 10);
    assert_eq!(output["truncated"], true);
    assert!(output["notice"]
        .as_str()
        .unwrap()
        .contains("Showing lines 1-3 of 10"));
    assert!(output["content"].as_str().unwrap().contains("line 1"));
    assert!(!output["content"].as_str().unwrap().contains("line 4"));
}

#[test]
fn execute_agent_tool_dispatches_fs_read_through_auto_execution() {
    let root = temp_workspace("dispatch-fs-read");
    let target = root.join("file.txt");
    fs::write(&target, "hello").unwrap();
    let workspace = Workspace::mvp();
    let tool_state = ToolSessionState::new(root);
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.read".to_string(),
        input: json!({ "path": target.display().to_string() }),
    };

    let output = tool_output(execute_agent_tool(&workspace, &tool_state, &request));

    assert!(!is_error(&output));
    assert!(output["content"].as_str().unwrap().contains("hello"));
}

// --- fs.write ------------------------------------------------------------

#[test]
fn fs_write_creates_parent_dirs_for_new_file() {
    let root = temp_workspace("write-new");
    let target = root.join("nested").join("dir").join("file.txt");
    let tool_state = ToolSessionState::new(root);

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
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

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
        &json!({ "path": target.display().to_string(), "content": "overwritten" }),
    );

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("has not been read"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[test]
fn fs_write_overwrite_after_read_succeeds() {
    let root = temp_workspace("write-after-read");
    let target = root.join("existing.txt");
    fs::write(&target, "original").unwrap();
    let tool_state = ToolSessionState::new(root);

    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
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

    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );
    fs::write(&target, "changed underneath").unwrap();
    bump_mtime(&target);

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
        &json!({ "path": target.display().to_string(), "content": "overwritten" }),
    );

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("changed on disk"));
}

#[test]
fn fs_write_rejects_new_file_path_escaping_workspace_root() {
    let root = temp_workspace("write-escape-root");
    let outside = temp_workspace("write-escape-outside");
    let target = outside.join("new.txt");
    let tool_state = ToolSessionState::new(root);

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
        &json!({ "path": target.display().to_string(), "content": "x" }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("escapes"));
    assert!(!target.exists());
}

// --- fs.edit ---------------------------------------------------------------

#[test]
fn fs_edit_without_prior_read_is_error() {
    let root = temp_workspace("edit-unread");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "world", "new_string": "there" }),
    );

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("has not been read"));
}

#[test]
fn fs_edit_zero_matches_is_error() {
    let root = temp_workspace("edit-zero-matches");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "not present", "new_string": "x" }),
    );

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("not found"));
}

#[test]
fn fs_edit_multiple_matches_is_error() {
    let root = temp_workspace("edit-multi-matches");
    let target = root.join("file.txt");
    fs::write(&target, "dup dup dup").unwrap();
    let tool_state = ToolSessionState::new(root);
    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "dup", "new_string": "x" }),
    );

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("found 3 matches"));
}

#[test]
fn fs_edit_success_updates_mtime_and_allows_chained_edit() {
    let root = temp_workspace("edit-chain");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );

    let first = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "world", "new_string": "there" }),
    );
    assert!(!is_error(&first));
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello there");

    // No re-read in between: the edit above must have refreshed the
    // recorded mtime itself, or this would fail the staleness gate.
    let second = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "there", "new_string": "again" }),
    );
    assert!(!is_error(&second));
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello again");
}

#[test]
fn fs_edit_stale_after_external_modification_is_error() {
    let root = temp_workspace("edit-stale");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    );

    fs::write(&target, "hello world, modified externally").unwrap();
    bump_mtime(&target);

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.edit",
        &json!({ "path": target.display().to_string(), "old_string": "world", "new_string": "there" }),
    );

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("changed on disk"));
}

// --- fs.glob / fs.grep ------------------------------------------------

#[test]
fn fs_glob_bounds_results_and_reports_total_count() {
    let root = temp_workspace("glob-bounded");
    for index in 0..5 {
        fs::write(root.join(format!("file-{index}.txt")), "content").unwrap();
    }
    let tool_state = ToolSessionState::new(root.clone());

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt", "limit": 2 }),
    )
    .expect("fs.glob is auto-executed");

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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "TODO", "limit": 2 }),
    )
    .expect("fs.grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["returned_count"], 2);
    assert_eq!(output["total_matches"], 5);
    assert_eq!(output["truncated"], true);
}

#[test]
fn fs_grep_rejects_invalid_regex() {
    let root = temp_workspace("grep-invalid-regex");
    let tool_state = ToolSessionState::new(root.clone());

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "(unclosed" }),
    )
    .expect("fs.grep is auto-executed");

    assert!(is_error(&output));
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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt" }),
    )
    .expect("fs.glob is auto-executed");

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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "content" }),
    )
    .expect("fs.grep is auto-executed");

    assert!(!is_error(&output));
    assert_eq!(output["total_matches"], 1);
    assert_eq!(
        output["matches"][0]["path"].as_str().unwrap(),
        root.join("keep.txt").display().to_string()
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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.txt" }),
    )
    .expect("fs.glob is auto-executed");

    assert!(!is_error(&output));
    let total_matches = output["total_matches"].as_u64().unwrap();
    assert!(
        total_matches < 25,
        "expected the file-count cap to stop the scan early, got {total_matches}"
    );
    assert!(output["note"]
        .as_str()
        .expect("note present when the scan is truncated")
        .contains("scan truncated"));
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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.grep",
        &json!({ "base_path": root.display().to_string(), "pattern": "x" }),
    )
    .expect("fs.grep is auto-executed");

    assert!(!is_error(&output));
    let total_matches = output["total_matches"].as_u64().unwrap();
    assert!(
        total_matches < 10,
        "expected the byte cap to stop the scan before reading every file, got {total_matches}"
    );
    assert!(output["note"]
        .as_str()
        .expect("note present when the scan is truncated")
        .contains("scan truncated"));
}

// --- approval wiring -----------------------------------------------------

fn requested_frame(call_id: &ToolCallId, tool_id: &str, input: serde_json::Value) -> AgentFrame {
    let mut frame = AgentFrame::empty();
    frame
        .items
        .push(AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: tool_id.to_string(),
            input,
        }));
    frame
}

#[test]
fn resolve_approval_forwards_non_horizon_executed_tools() {
    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(&call_id, "mock.approval_required", json!({}));
    let session_id = SessionId::new();

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );

    assert!(matches!(
        outcome,
        ApprovalOutcome::Forward(Command::ApproveToolCall { call_id: id }) if id == call_id
    ));
}

#[test]
fn resolve_approval_forwards_when_no_runtime_registered() {
    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(
        &call_id,
        "fs.write",
        json!({ "path": "/tmp/does-not-matter.txt", "content": "x" }),
    );
    // A fresh session id that was never registered via `register_session_runtime`.
    let session_id = SessionId::new();

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Deny { reason: None },
    );

    assert!(matches!(
        outcome,
        ApprovalOutcome::Forward(Command::DenyToolCall { call_id: id, .. }) if id == call_id
    ));
}

#[test]
fn resolve_approval_executes_fs_write_on_approve() {
    let root = temp_workspace("approval-write");
    let target = root.join("new.txt");
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    register_session_runtime(
        session_id,
        tool_state,
        LiveState::new(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(
        &call_id,
        "fs.write",
        json!({ "path": target.display().to_string(), "content": "approved content" }),
    );

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Executed { frame, command } = outcome else {
        panic!("expected fs.write to be executed by Horizon");
    };
    assert!(matches!(
        &command,
        Command::ToolCallResult(result) if result.call_id == call_id && result.output["is_error"].is_null()
    ));
    assert_eq!(fs::read_to_string(&target).unwrap(), "approved content");
    assert!(frame.items.iter().any(|item| matches!(
        item,
        AgentFrameItem::ToolCallFinished(result) if result.call_id == call_id
    )));
}

#[test]
fn resolve_approval_denies_fs_edit_without_running_it() {
    let root = temp_workspace("approval-deny-edit");
    let target = root.join("file.txt");
    fs::write(&target, "hello world").unwrap();
    let tool_state = ToolSessionState::new(root);
    let session_id = SessionId::new();
    register_session_runtime(
        session_id,
        tool_state,
        LiveState::new(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("call-1".to_string());
    let frame = requested_frame(
        &call_id,
        "fs.edit",
        json!({ "path": target.display().to_string(), "old_string": "world", "new_string": "there" }),
    );

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Deny { reason: None },
    );

    let ApprovalOutcome::Executed { command, .. } = outcome else {
        panic!("expected fs.edit denial to be resolved by Horizon");
    };
    let Command::ToolCallResult(result) = command else {
        panic!("expected a ToolCallResult command");
    };
    assert_eq!(result.output["is_error"], true);
    assert_eq!(result.output["message"], "denied by user");
    // Never ran: the file is untouched.
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
        tool_id: "fs.write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "first" }),
    })]);

    let first = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );
    let ApprovalOutcome::Executed {
        frame: updated_frame,
        ..
    } = first
    else {
        panic!("first approve should execute fs.write");
    };
    assert_eq!(fs::read_to_string(&target).unwrap(), "first");

    // Prove the duplicate doesn't re-run the tool: change the file on disk;
    // a re-executed write would clobber (or error on) this content.
    fs::write(&target, "externally changed").unwrap();

    let second = resolve_approval(
        &updated_frame,
        session_id,
        call_id,
        ApprovalDecision::Approve,
    );
    assert!(matches!(second, ApprovalOutcome::AlreadyResolved));
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
        tool_id: "fs.write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "should never land" }),
    })]);

    let denied = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Deny { reason: None },
    );
    let ApprovalOutcome::Executed {
        frame: updated_frame,
        ..
    } = denied
    else {
        panic!("deny of fs.write should be resolved by Horizon");
    };
    assert!(!target.exists());

    let approved_late = resolve_approval(
        &updated_frame,
        session_id,
        call_id,
        ApprovalDecision::Approve,
    );
    assert!(matches!(approved_late, ApprovalOutcome::AlreadyResolved));
    assert!(!target.exists());
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
        input: json!({ "command": "echo hi" }),
    })]);

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Started { frame } = outcome else {
        panic!("approving bash should start it, not finish it synchronously");
    };
    assert!(frame
        .items
        .iter()
        .any(|item| matches!(item, AgentFrameItem::ToolCallStarted(id) if id == &call_id)));
    assert!(!frame.has_tool_call_finished(&call_id));

    // The command actually ran on a background thread and its result
    // arrives on the channel `resolve_approval` was given.
    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the approved bash call should finish and report a result");
    assert_eq!(completion.result.call_id, call_id);
    assert_eq!(completion.result.output["exit_code"], 0);
    assert_eq!(completion.result.output["output"], "hi\n");
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
        input: json!({ "command": "echo should-never-run" }),
    })]);

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Deny { reason: None },
    );

    let ApprovalOutcome::Executed { command, .. } = outcome else {
        panic!("denying bash should resolve synchronously, like fs.write/fs.edit");
    };
    let Command::ToolCallResult(result) = command else {
        panic!("expected a ToolCallResult command");
    };
    assert_eq!(result.output["is_error"], true);
    assert_eq!(result.output["message"], "denied by user");

    // Never spawned: nothing ever arrives on the results channel.
    assert!(bash_results_rx
        .recv_timeout(std::time::Duration::from_millis(200))
        .is_err());
}

#[test]
fn tool_session_state_seeds_bash_cwd_from_the_workspace_root() {
    let root = temp_workspace("bash-initial-cwd");
    let tool_state = ToolSessionState::new(root.clone());

    assert_eq!(tool_state.bash_cwd(), root);
    assert_eq!(*tool_state.bash_cwd_handle().lock().unwrap(), root);
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

    let output = fs_tools::execute_auto(&tool_state, "fs.read", &json!({ "path": requested }))
        .expect("fs.read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"].as_str().unwrap().contains("`..`"));
}

#[test]
fn fs_read_normalizes_cur_dir_components() {
    let root = temp_workspace("read-cur-dir");
    fs::write(root.join("file.txt"), "hello").unwrap();
    let tool_state = ToolSessionState::new(root.clone());
    let requested = format!("{}/./file.txt", root.display());

    let output = fs_tools::execute_auto(&tool_state, "fs.read", &json!({ "path": requested }))
        .expect("fs.read is auto-executed");

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

    let output = fs_tools::execute_approved(
        &tool_state,
        "fs.write",
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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": file.display().to_string() }),
    )
    .expect("fs.read is auto-executed");

    assert!(is_error(&output));
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("workspace root"));
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
