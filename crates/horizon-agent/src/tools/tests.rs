use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::json;

use super::*;
use crate::contract::{
    Command, Event, ProviderEvent, SessionId, ToolCallId, ToolCallRequest, ToolCallResult,
    ToolPermission,
};
use crate::frame::{AgentFrame, AgentFrameItem};
use crate::live::LiveState;
use crate::tools::execution::{execute_agent_tool, HostTools};
use crate::tools::fs as fs_tools;
use crate::tools::state::{
    register_session_runtime, session_runtime, unregister_session_runtime, ToolSessionState,
};

/// A `HostTools` stub for tests that need *some* auto-allow host tool to
/// exercise dispatch/processing plumbing, but don't care about real
/// Horizon-side output — the real `workspace.snapshot` implementation lives
/// in Horizon (`agent::host_tools`) since it reads `Workspace`, a type this
/// crate can't depend on (see `docs/agent-runtime-split-design.md`). Returns
/// a fixed, empty snapshot for `workspace.snapshot` and `None` for anything
/// else, so unrelated tool ids still fall through to `tools::fs`.
struct StubHostTools;

impl HostTools for StubHostTools {
    fn execute_auto(&self, tool_id: &str, _input: &serde_json::Value) -> Option<serde_json::Value> {
        match tool_id {
            "workspace.snapshot" => Some(json!({ "stub": true })),
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
/// receiver — see `horizon-sessiond`'s session loop,
/// `crates/horizon-sessiond/src/session.rs`).
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

/// Unwraps a completion expected to be finished -- every bash test in this
/// module exercises the plain (unsandboxed) manual-approval path, which
/// never produces a retry-without-sandbox prompt.
fn expect_finished(completion: BashCompletion) -> ToolCallResult {
    match completion {
        BashCompletion::Finished(result) => result,
        BashCompletion::RetryWithoutSandbox { call_id, reason } => panic!(
            "expected a finished bash completion, got a retry-without-sandbox \
             request for {call_id:?}: {reason}"
        ),
        BashCompletion::DomainDenied {
            call_id, domains, ..
        } => panic!(
            "expected a finished bash completion, got a domain-denied request for \
             {call_id:?} ({domains:?})"
        ),
    }
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
    let tool_state = ToolSessionState::new(root);
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.read".to_string(),
        input: json!({ "path": target.display().to_string() }),
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
//
// The 2026-07-19 dogfooding bug: the model called a nonexistent `write` tool
// (the catalog id is `fs.write`); a real `Event::ApprovalRequested` reached
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
        input: json!({ "path": "/tmp/x", "content": "hi" }),
    };

    let Execution::Unknown(events) =
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
        message.contains("fs.write"),
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
            input: json!({}),
        }),
    );

    assert!(
        !processing
            .horizon_events
            .iter()
            .any(|event| matches!(event.event, Event::ApprovalRequested(_))),
        "an unknown tool id must never reach a human approval prompt: {:?}",
        processing.horizon_events
    );

    let result = processing
        .horizon_events
        .iter()
        .find_map(|event| match &event.event {
            Event::ToolCallFinished(result) => Some(result.clone()),
            _ => None,
        })
        .expect("expected a ToolCallFinished error result the model can see");
    assert!(is_error(&result.output));

    // The turn continues: the provider gets the result back as a real
    // `Command::ToolCallResult`, exactly like any other tool's outcome, so
    // the model can correct itself instead of the turn stalling forever.
    assert!(processing
        .provider_commands
        .iter()
        .any(|command| matches!(command, Command::ToolCallResult(r) if r.call_id == call_id)));
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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*" }),
    )
    .expect("fs.glob is auto-executed");

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

/// Locks in `walk`'s `require_git` choice (the `ignore` crate's default):
/// a `.gitignore` file only takes effect inside an actual git repository,
/// not in an arbitrary directory that merely happens to contain one. A
/// future change relaxing this would silently start hiding matches for
/// non-git workspace roots.
#[test]
fn fs_glob_ignores_gitignore_file_outside_a_git_repository() {
    let root = temp_workspace("glob-gitignore-no-git");
    fs::write(root.join(".gitignore"), "secret.log\n").unwrap();
    fs::write(root.join("secret.log"), "content").unwrap();
    let tool_state = ToolSessionState::new(root.clone());

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*.log" }),
    )
    .expect("fs.glob is auto-executed");

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

    let output = fs_tools::execute_auto(
        &tool_state,
        "fs.glob",
        &json!({ "base_path": root.display().to_string(), "pattern": "*" }),
    )
    .expect("fs.glob is auto-executed");

    assert!(!is_error(&output));
    assert!(output["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str().unwrap().ends_with(".eslintrc.json")));
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

    let ApprovalOutcome::Executed { frame, command, .. } = outcome else {
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
    assert!(
        result.denied,
        "the contract-explicit denial marker must be set"
    );
    // Never ran: the file is untouched.
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
}

#[test]
fn resolve_approval_approve_that_fails_on_its_own_does_not_set_the_denied_marker() {
    // Distinguishes a genuine denial from an *approved* call that fails for
    // its own reasons (fs.edit's "old_string not found" here): both are
    // `is_error: true`, but only a denial sets `ToolCallResult::denied`.
    let root = temp_workspace("approval-approve-then-fail");
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
    // No prior `fs.read` for this path -- `fs.edit` requires one, so this
    // approve runs the tool (it's not a denial) but the tool itself fails.
    let frame = requested_frame(
        &call_id,
        "fs.edit",
        json!({ "path": target.display().to_string(), "old_string": "world", "new_string": "there" }),
    );

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );

    let ApprovalOutcome::Executed { command, .. } = outcome else {
        panic!("expected fs.edit approve to resolve synchronously");
    };
    let Command::ToolCallResult(result) = command else {
        panic!("expected a ToolCallResult command");
    };
    assert_eq!(result.output["is_error"], true);
    assert!(
        !result.denied,
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

#[test]
fn resolve_approval_executes_a_new_occurrence_of_a_reused_call_id() {
    // Root-caused 2026-07-18 (a real owner session, rig/OpenAI-compatible
    // provider): the provider reused the exact same call_id string for two
    // structurally different `fs.write`/`fs.edit` calls in one session --
    // the first fully resolved (approved and finished) well before the
    // model ever requested the second. The old `has_tool_call_finished`/
    // `has_tool_call_started` whole-session scan mistook the *first*
    // occurrence's finish for the *second* occurrence's, so `try_execute`
    // permanently returned `AlreadyResolved` for a call nobody had ever
    // acted on -- no tool ran, no result reached the provider, the turn
    // wedged forever with no way to recover (the daemon-side half of the
    // "session stuck, no working Approve" report; the UI-side half is
    // `src/agent/turns.rs`'s `build_tool_call_views`/`tool_call_body`).
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
        tool_id: "fs.write".to_string(),
        input: json!({ "path": target_a.display().to_string(), "content": "first" }),
    })]);
    let first = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );
    assert!(
        matches!(first, ApprovalOutcome::Executed { .. }),
        "first occurrence's approve should execute fs.write"
    );
    assert_eq!(fs::read_to_string(&target_a).unwrap(), "first");

    // Second occurrence: the provider reuses `call_id` for a genuinely
    // different, later call.
    let after_second_request =
        live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "fs.write".to_string(),
            input: json!({ "path": target_b.display().to_string(), "content": "second" }),
        })]);

    let second = resolve_approval(
        &after_second_request,
        session_id,
        call_id,
        ApprovalDecision::Approve,
    );
    assert!(
        matches!(second, ApprovalOutcome::Executed { .. }),
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
    let tool_state = ToolSessionState::new(root.clone()).with_isolated_worktree(true);
    let target = root.join("new.txt");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "hi" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    let Execution::Auto(events) = execution else {
        panic!("expected Execution::Auto for a contained fs.write");
    };
    assert!(events
        .iter()
        .any(|event| matches!(event, Event::ToolCallStarted(id) if id == &request.call_id)));
    let output = events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output),
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
    let tool_state = ToolSessionState::new(root).with_isolated_worktree(true);
    // fs.edit's staleness gate needs a prior fs.read recorded -- mirrors
    // `fs_edit_success_updates_mtime_and_allows_chained_edit` above.
    fs_tools::execute_auto(
        &tool_state,
        "fs.read",
        &json!({ "path": target.display().to_string() }),
    )
    .expect("fs.read is auto-executed");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.edit".to_string(),
        input: json!({ "path": target.display().to_string(), "old_string": "before", "new_string": "after" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    assert!(
        matches!(execution, Execution::Auto(_)),
        "expected Execution::Auto for a contained fs.edit, got {execution:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "after");
}

#[test]
fn fs_write_still_requires_approval_when_the_session_is_not_isolated() {
    let root = temp_workspace("tier1-fs-write-not-isolated");
    let tool_state = ToolSessionState::new(root.clone()); // isolation defaults to false
    let target = root.join("new.txt");
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.write".to_string(),
        input: json!({ "path": target.display().to_string(), "content": "hi" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    assert_eq!(execution, Execution::RequiresApproval);
    assert!(!target.exists(), "must not have run yet");
}

#[test]
fn horizon_events_for_provider_event_omits_the_approval_prompt_for_a_contained_fs_write() {
    let tool_state = ToolSessionState::new(std::env::temp_dir()).with_isolated_worktree(true);
    let events = crate::policy::horizon_events_for_provider_event(
        &Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs.write".to_string(),
            input: json!({ "path": "/tmp/x", "content": "hi" }),
        }),
        &tool_state,
        SessionId::new(),
    );
    assert_eq!(events.len(), 1, "no approval prompt expected: {events:?}");
}

/// The real thing this whole leg exists for: a `bash` call in an isolated
/// session, on a host where `horizon_sandbox::is_available()` is genuinely
/// true (this dev machine has bwrap -- see AGENTS.md), auto-executes
/// *sandboxed* on the background thread, and its eventual result carries
/// both audit markers (`sandboxed`, `auto_approved`/tier/reason).
#[test]
fn bash_auto_executes_sandboxed_in_an_isolated_session_with_an_engaged_sandbox() {
    let root = temp_workspace("tier1-bash-sandboxed");
    let tool_state = ToolSessionState::new(root).with_isolated_worktree(true);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    let Execution::Started(events) = execution else {
        panic!("expected Execution::Started for a contained bash call");
    };
    assert!(events
        .iter()
        .any(|event| matches!(event, Event::ToolCallStarted(id) if id == &request.call_id)));

    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the sandboxed bash call should finish");
    let result = expect_finished(completion);
    assert_eq!(result.call_id, request.call_id);
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "hi\n");
    assert_eq!(result.output["sandboxed"], true, "{:?}", result.output);
    assert_eq!(result.output["auto_approved"], true);
    assert_eq!(result.output["policy_tier"], "contained");

    unregister_session_runtime(session_id);
}

/// A sandboxed tier-1 call that overruns its timeout must still be killed
/// and report promptly -- proving `wait_child_with_timeout`'s pid-based
/// kill actually tears down a real bwrap-contained child, not just the
/// unsandboxed process-group path `timeout_kills_the_process_and_reports_
/// captured_partial_output` (`bash::tests`) already covers.
#[test]
fn bash_auto_executes_sandboxed_and_is_killed_on_timeout() {
    let root = temp_workspace("tier1-bash-sandboxed-timeout");
    let tool_state = ToolSessionState::new(root).with_isolated_worktree(true);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo start; sleep 5", "timeout_secs": 1 }),
    };

    let started = std::time::Instant::now();
    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    assert!(matches!(execution, Execution::Started(_)));

    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the sandboxed bash call should be killed and report promptly");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "should be killed well before the full 5s sleep completes"
    );
    let result = expect_finished(completion);
    assert_eq!(result.output["is_error"], true);
    assert!(result.output["message"]
        .as_str()
        .expect("message")
        .contains("timed out"));
    assert_eq!(result.output["sandboxed"], true);

    unregister_session_runtime(session_id);
}

/// The crux regression test for the 2026-07-19 dogfooding containment hole:
/// a live session ran a tier-1 auto-approved, sandboxed `bash` call
/// (`echo outside > /tmp/horizon-dogfood-boundary.txt`) and the file showed
/// up on the *host's real* `/tmp`, even though the result carried
/// `sandboxed: true`. Root cause was `run_sandboxed` adding the host's own
/// `std::env::temp_dir()` as a second writable root, which (on the then-
/// bwrap Linux backend) bind-mounted the shared host `/tmp` directly over
/// bwrap's private `--tmpfs /tmp`, undoing it (see
/// `tools::bash::exec::run_sandboxed`'s doc comment). This drives the exact
/// reported command through the full product path (`execute_agent_tool` ->
/// tier-1 dispatch -> the real background bash thread -> the real sandbox
/// backend) and asserts the file never lands on the host.
///
/// Behavior updated for the nono/Landlock backend migration
/// (`docs/roadmap.md`'s backlog-60 entry): nono has no mount namespace, so
/// there is no private tmpfs for a literal `/tmp` write to land in (a
/// deliberate, accepted behavior change -- see `horizon_sandbox::linux::
/// spawn`'s TMPDIR-parity comment). The write is now denied outright
/// (`is_likely_sandbox_denied` classifies it as sandbox-denied), so this
/// tier-1 call surfaces `BashCompletion::RetryWithoutSandbox` instead of
/// finishing successfully -- still never landing on the host's real `/tmp`,
/// just via denial rather than a silently-redirected private overlay.
#[test]
fn tier1_sandboxed_bash_write_to_tmp_never_leaks_to_the_hosts_real_tmp() {
    let root = temp_workspace("tier1-bash-sandboxed-tmp-leak");
    let tool_state = ToolSessionState::new(root).with_isolated_worktree(true);
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    let (bash_results_tx, bash_results_rx) = crossbeam_channel::unbounded();
    register_session_runtime(session_id, tool_state.clone(), live_state, bash_results_tx);

    let marker = format!(
        "horizon-dogfood-boundary-regression-{}.txt",
        uuid::Uuid::new_v4()
    );
    let host_target = std::path::Path::new("/tmp").join(&marker);
    let _ = std::fs::remove_file(&host_target);

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": format!("echo outside > {}", host_target.display()) }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, session_id, &request);
    assert!(matches!(execution, Execution::Started(_)));

    let completion = bash_results_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the sandboxed bash call should finish");

    match completion {
        BashCompletion::RetryWithoutSandbox { call_id, reason } => {
            assert_eq!(call_id, request.call_id);
            assert!(
                reason.contains("denied"),
                "expected a sandbox-denial reason: {reason}"
            );
        }
        BashCompletion::Finished(result) => {
            panic!(
                "expected the literal /tmp write to be denied by the sandbox \
                 (no private tmpfs under nono), got a finished result instead: {:?}",
                result.output
            );
        }
        BashCompletion::DomainDenied {
            call_id, domains, ..
        } => {
            panic!(
                "expected the literal /tmp write to be denied by the sandbox, got a \
                 domain-denied request instead for {call_id:?} ({domains:?})"
            );
        }
    }
    assert!(
        !host_target.exists(),
        "a tier-1 sandboxed bash write to /tmp must never leak onto the host's \
         real /tmp -- the exact 2026-07-19 dogfooding containment hole"
    );

    let _ = std::fs::remove_file(&host_target);
    unregister_session_runtime(session_id);
}

// --- tier-1 bash containment: network proxy (leg 4a) -----------------------
//
// The two containment tests that used to live here (empty-allowlist decoy
// refusal, direct-egress-under-Proxied) moved to
// `tests/tier1_network_containment.rs`, an integration test: they need
// `env!("CARGO_BIN_EXE_bridge_probe")`, which Cargo only bakes in for
// integration-test/bench/example compilation units, never a crate's own lib
// unit tests (confirmed the hard way -- a runtime-resolved fallback path
// passed locally against a build that happened to already have the binary,
// then failed deterministically on a clean `cargo clean -p horizon-agent &&
// cargo nextest run`, since nothing guarantees `[[bin]]` targets are built
// before lib unit tests run). `execute_agent_tool`/`Execution` are
// re-exported `pub` from `tools::mod` (narrowly, just those two) so that
// external test can still drive the real tier-1 dispatch path.

/// Never-silently-degrade: even on a host where the sandbox genuinely
/// engages, a *non-isolated* session's `bash` call still goes through the
/// ordinary approval gate -- isolation is load-bearing, not cosmetic. The
/// complementary "isolated but no engaged sandbox" case is
/// `policy::tests::bash_is_contained_only_when_isolated_and_sandboxed`,
/// exercised directly against the pure predicate since this dev machine
/// can't be made to lack bwrap for an integration-level test.
#[test]
fn bash_requires_approval_when_the_session_is_not_isolated() {
    let root = temp_workspace("tier1-bash-not-isolated");
    let tool_state = ToolSessionState::new(root); // isolation defaults to false
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }),
    };

    let execution = execute_agent_tool(&StubHostTools, &tool_state, SessionId::new(), &request);
    assert_eq!(execution, Execution::RequiresApproval);
}

/// The denial-retry flow's frame-level mechanic
/// (`docs/agent-approval-design.md`'s "Denial UX"): `horizon-sessiond`'s
/// `fold_bash_retry_without_sandbox` reissues a fresh `ToolCallRequested`
/// for the same `call_id` right before a fresh `ApprovalRequested`, after a
/// first (sandboxed) attempt already folded `ToolCallStarted` but never
/// `ToolCallFinished`. This proves that reissue is what makes the retry's
/// own eventual Approve resolve normally (`Started`), instead of being
/// misclassified as `AlreadyResolved` by `tools::approval::try_execute`'s
/// idempotence guard -- without needing the real sandboxed exec plumbing.
#[test]
fn resolve_approval_accepts_a_denial_retry_reissue_and_runs_it_unsandboxed() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("bash-retry".to_string());

    // First (sandboxed) attempt: requested, started -- never finished (the
    // sandboxed job detected a denial instead of completing normally).
    live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "bash".to_string(),
        input: json!({ "command": "echo hi" }),
    })]);
    let after_started = live_state.extend_events([Event::ToolCallStarted(call_id.clone())]);
    assert!(after_started.has_tool_call_started(&call_id));
    assert!(!after_started.has_tool_call_finished(&call_id));

    // The reissue: a fresh `ToolCallRequested` for the same call_id, then
    // the normal approval-request events -- exactly what
    // `fold_bash_retry_without_sandbox` folds in `horizon-sessiond`.
    let after_retry_request = live_state.extend_events([
        Event::ToolCallRequested(ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "bash".to_string(),
            input: json!({ "command": "echo hi" }),
        }),
        Event::ApprovalRequested(crate::contract::ApprovalRequest {
            call_id: call_id.clone(),
            reason: "sandboxed run looked denied".to_string(),
            kind: crate::contract::ApprovalKind::SandboxDenialRetry,
        }),
    ]);
    assert!(
        !after_retry_request.has_tool_call_started(&call_id),
        "the reissue must reset the started/finished scope, or the retry's \
         own approve would be misclassified as AlreadyResolved"
    );

    let outcome = resolve_approval(
        &after_retry_request,
        session_id,
        call_id,
        ApprovalDecision::Approve,
    );
    assert!(
        matches!(outcome, ApprovalOutcome::Started { .. }),
        "the retry's approve must actually start the call, not be dropped \
         as AlreadyResolved: {outcome:?}"
    );
}

// --- approval wiring: domain-denial retry (leg 4b) -------------------------

fn domain_denial_retry_frame(
    live_state: &LiveState,
    call_id: &ToolCallId,
    domains: Vec<String>,
) -> (AgentFrame, ToolCallResult) {
    live_state.extend_events([Event::ToolCallRequested(ToolCallRequest {
        call_id: call_id.clone(),
        tool_id: "bash".to_string(),
        input: json!({ "command": "curl https://example.com" }),
    })]);

    let prior_result = ToolCallResult::new(
        call_id.clone(),
        json!({ "is_error": true, "denied_domains": domains, "exit_code": 0 }),
    );
    let frame =
        live_state.extend_events([Event::ApprovalRequested(crate::contract::ApprovalRequest {
            call_id: call_id.clone(),
            reason: "allow example.com for this session and retry?".to_string(),
            kind: crate::contract::ApprovalKind::DomainDenialRetry {
                domains: vec!["example.com".to_string()],
                prior_result: prior_result.clone(),
            },
        })]);
    (frame, prior_result)
}

/// The domain-denial-retry flow's deny path (`docs/agent-approval-
/// design.md` leg 4b): unlike an ordinary bash deny (which synthesizes a
/// fresh "denied by user" marker), this forwards the already-computed
/// `prior_result` unchanged -- the call already ran to completion, it just
/// couldn't reach some host, and that outcome already reflects the denial.
#[test]
fn resolve_approval_domain_denial_retry_deny_forwards_the_prior_result_unchanged() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("bash-domain-retry-deny".to_string());
    let (frame, prior_result) =
        domain_denial_retry_frame(&live_state, &call_id, vec!["example.com".to_string()]);

    let outcome = resolve_approval(
        &frame,
        session_id,
        call_id,
        ApprovalDecision::Deny { reason: None },
    );
    match outcome {
        ApprovalOutcome::Executed { command, .. } => {
            assert_eq!(command, Command::ToolCallResult(prior_result));
        }
        other => panic!("expected Executed forwarding the prior result, got {other:?}"),
    }
}

/// The domain-denial-retry flow's approve path, defensive branch: a
/// `DomainDenialRetry` should only ever be produced for a tier-1 sandboxed
/// call (which always has a `SessionNetworkProxy` attached), but if this
/// session somehow has none, approve must not panic or silently drop the
/// decision -- it falls back to forwarding the already-computed result,
/// exactly like a deny would.
#[test]
fn resolve_approval_domain_denial_retry_approve_without_a_network_proxy_falls_back_to_forwarding() {
    let tool_state = dummy_tool_state();
    let session_id = SessionId::new();
    let live_state = LiveState::new();
    register_session_runtime(
        session_id,
        tool_state,
        live_state.clone(),
        dummy_bash_results(),
    );

    let call_id = ToolCallId("bash-domain-retry-approve-no-proxy".to_string());
    let (frame, prior_result) =
        domain_denial_retry_frame(&live_state, &call_id, vec!["example.com".to_string()]);

    let outcome = resolve_approval(&frame, session_id, call_id, ApprovalDecision::Approve);
    match outcome {
        ApprovalOutcome::Executed { command, .. } => {
            assert_eq!(command, Command::ToolCallResult(prior_result));
        }
        other => panic!("expected a defensive fallback forwarding the prior result, got {other:?}"),
    }
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

    let ApprovalOutcome::Started { frame, .. } = outcome else {
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
    let result = expect_finished(completion);
    assert_eq!(result.call_id, call_id);
    assert_eq!(result.output["exit_code"], 0);
    assert_eq!(result.output["output"], "hi\n");
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
    assert!(
        result.denied,
        "the contract-explicit denial marker must be set"
    );

    // Never spawned: nothing ever arrives on the results channel.
    assert!(bash_results_rx
        .recv_timeout(std::time::Duration::from_millis(200))
        .is_err());
}

/// Regression test for the 2026-07 repeated-approval OOM incident's root
/// cause: `resolve_approval_second_approve_is_noop` (above) proves the
/// `AlreadyResolved` guard for `fs.write`, whose `ToolCallStarted` and
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
        input: json!({ "command": "echo first" }),
    })]);

    let first = resolve_approval(
        &frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );
    let ApprovalOutcome::Started {
        frame: running_frame,
        ..
    } = first
    else {
        panic!("first approve should start bash");
    };
    // The frame the second approve would actually see (production folds
    // this exact frame, e.g. `horizon-sessiond`'s `resolve_and_forward` reads
    // `live_state.frame()` fresh for every inbound command) already shows
    // `ToolCallStarted` but not yet `ToolCallFinished` -- the vulnerable
    // window.
    assert!(running_frame.has_tool_call_started(&call_id));
    assert!(!running_frame.has_tool_call_finished(&call_id));

    let second = resolve_approval(
        &running_frame,
        session_id,
        call_id.clone(),
        ApprovalDecision::Approve,
    );
    assert!(
        matches!(second, ApprovalOutcome::AlreadyResolved),
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
