use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::BashToolConfig;
use crate::contract::{ToolCallId, ToolCallResult};
use crate::frame::{AgentFrame, AgentFrameItem};

fn cwd_handle(path: PathBuf) -> Arc<Mutex<PathBuf>> {
    Arc::new(Mutex::new(path))
}

fn config() -> BashToolConfig {
    BashToolConfig::default()
}

// --- basic execution ------------------------------------------------------

#[test]
fn echo_round_trip_reports_output_and_exit_zero() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("echo-1".to_string());

    let output = super::exec::run(
        &call_id,
        &json!({ "command": "echo hello" }),
        &cwd,
        &config(),
    );

    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["output"], "hello\n");
    assert_eq!(output["truncated"], false);
    assert!(output.get("is_error").is_none());
    assert!(output["output_file"].as_str().is_some());
}

#[test]
fn non_zero_exit_is_a_normal_result_carrying_the_code() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("exit-7".to_string());

    let output = super::exec::run(&call_id, &json!({ "command": "exit 7" }), &cwd, &config());

    assert!(
        output.get("is_error").is_none(),
        "a non-zero exit is a normal result, not is_error: {output}"
    );
    assert_eq!(output["exit_code"], 7);
}

// --- timeout ----------------------------------------------------------

#[test]
fn timeout_kills_the_process_and_reports_captured_partial_output() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("timeout-1".to_string());

    let started = Instant::now();
    let output = super::exec::run(
        &call_id,
        &json!({ "command": "echo start; sleep 5", "timeout_secs": 1 }),
        &cwd,
        &config(),
    );

    assert!(
        started.elapsed() < Duration::from_secs(4),
        "should be killed well before the full 5s sleep completes"
    );
    assert_eq!(output["is_error"], true);
    assert!(output["message"]
        .as_str()
        .expect("message")
        .contains("timed out"));
    assert!(
        output["output"]
            .as_str()
            .expect("captured partial output")
            .contains("start"),
        "should include whatever was printed before the kill: {output}"
    );
}

// --- post-exit pipe holders -----------------------------------------------

#[test]
fn background_child_holding_the_pipe_does_not_hang_the_call() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("bg-pipe-holder".to_string());

    // The bash child exits immediately (bash doesn't wait for background
    // jobs), but the backgrounded sleep inherits the output pipe's write
    // end and holds it for 30s — so the pumps never see EOF on their own.
    // The bounded post-exit drain must cut this short. Grace shortened via
    // the test hook so the suite stays fast.
    let started = Instant::now();
    let output = super::exec::run_with_drain_grace(
        &call_id,
        &json!({ "command": "echo visible; sleep 30 &" }),
        &cwd,
        Duration::from_millis(200),
        &config(),
    );

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "must return promptly, not wait out the background child's 30s"
    );
    assert_eq!(output["exit_code"], 0);
    assert!(
        output["output"]
            .as_str()
            .expect("output")
            .contains("visible"),
        "output produced before the child exited must be captured: {output}"
    );
    assert!(output["note"]
        .as_str()
        .expect("a cut-short drain should be noted in the result")
        .contains("background"),);
    // The kill registration must not outlive the call.
    assert!(!super::registry::is_registered(&call_id));
}

// --- output capping and spilling -------------------------------------------

#[test]
fn truncation_preserves_head_and_tail_and_spills_full_output() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("truncate-1".to_string());

    let output = super::exec::run(
        &call_id,
        &json!({ "command": "head -c 40000 /dev/zero | tr '\\0' 'a'" }),
        &cwd,
        &config(),
    );

    assert_eq!(output["truncated"], true);
    let shown = output["output"].as_str().expect("shown output");
    assert!(shown.starts_with("aaaa"));
    assert!(shown.ends_with("aaaa"));
    assert!(shown.contains("truncated"));
    assert!(shown.len() < 40_000);

    let spill_path = output["output_file"].as_str().expect("spill file path");
    let spilled = std::fs::read_to_string(spill_path).expect("spilled file should be readable");
    assert_eq!(spilled.len(), 40_000);
    assert!(spilled.chars().all(|c| c == 'a'));
    let _ = std::fs::remove_file(spill_path);
}

// --- cwd tracking -----------------------------------------------------

#[test]
fn cwd_tracking_persists_a_cd_across_calls_with_no_sentinel_leakage() {
    let base = std::env::temp_dir().join(format!("horizon-bash-cwd-test-{}", uuid::Uuid::new_v4()));
    let sub = base.join("sub");
    std::fs::create_dir_all(&sub).expect("create test dirs");
    let cwd = cwd_handle(base.clone());

    let first = super::exec::run(
        &ToolCallId("cwd-1".to_string()),
        &json!({ "command": "cd sub && pwd" }),
        &cwd,
        &config(),
    );
    assert_eq!(first["exit_code"], 0);
    let first_output = first["output"].as_str().expect("first output");
    // Nothing but `pwd`'s own line: no cwd-tracking sentinel mixed in.
    assert!(first_output.trim_end().ends_with("sub"));
    assert_eq!(first_output, format!("{}\n", first_output.trim_end()));

    let reported_cwd = first_output.trim().to_string();

    let second = super::exec::run(
        &ToolCallId("cwd-2".to_string()),
        &json!({ "command": "pwd" }),
        &cwd,
        &config(),
    );
    let second_output = second["output"].as_str().expect("second output");
    assert_eq!(
        second_output.trim(),
        reported_cwd,
        "the second call should see the cwd the first call `cd`ed into"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn cwd_tracking_leaves_cwd_unchanged_when_the_command_never_cds() {
    let base = std::env::temp_dir().join(format!("horizon-bash-cwd-noop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create test dir");
    let cwd = cwd_handle(base.clone());

    let _ = super::exec::run(
        &ToolCallId("cwd-noop".to_string()),
        &json!({ "command": "echo hi" }),
        &cwd,
        &config(),
    );

    assert_eq!(*cwd.lock().unwrap(), base);
    let _ = std::fs::remove_dir_all(&base);
}

// --- kill registry ------------------------------------------------------

#[test]
fn kill_registry_entry_is_removed_after_normal_completion() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("registry-normal".to_string());
    let (tx, rx) = crossbeam_channel::unbounded();

    super::spawn(
        call_id.clone(),
        json!({ "command": "true" }),
        cwd,
        config(),
        tx,
    );

    let completion = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("bash call should finish");
    assert_eq!(completion.result.call_id, call_id);
    assert!(!super::registry::is_registered(&call_id));
}

#[test]
fn kill_registry_kills_a_running_child_and_removes_its_entry() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("registry-kill".to_string());
    let (tx, rx) = crossbeam_channel::unbounded();

    super::spawn(
        call_id.clone(),
        json!({ "command": "sleep 10", "timeout_secs": 30 }),
        cwd,
        config(),
        tx,
    );

    let mut registered = false;
    for _ in 0..100 {
        if super::registry::is_registered(&call_id) {
            registered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(registered, "the child should have registered itself by now");

    assert!(
        super::registry::kill(&call_id),
        "kill should find the registered child"
    );
    assert!(!super::registry::is_registered(&call_id));

    let completion = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a killed bash call should still report a result promptly");
    assert_eq!(completion.result.output["is_error"], true);
}

// --- late-result idempotence --------------------------------------------

#[test]
fn should_fold_completion_is_false_once_the_call_already_has_a_finish() {
    let call_id = ToolCallId("late-result".to_string());
    let mut frame = AgentFrame::empty();

    assert!(super::should_fold_completion(&frame, &call_id));

    frame
        .items
        .push(AgentFrameItem::ToolCallFinished(ToolCallResult {
            call_id: call_id.clone(),
            output: json!({ "cancelled": true }),
        }));

    assert!(
        !super::should_fold_completion(&frame, &call_id),
        "a late result must be discarded once the frame already has a finish for the call"
    );
}

// --- lossy UTF-8 --------------------------------------------------------

#[test]
fn lossy_non_utf8_output_does_not_panic() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("non-utf8".to_string());

    let output = super::exec::run(
        &call_id,
        &json!({ "command": r"printf 'a\xffb'" }),
        &cwd,
        &config(),
    );

    assert_eq!(output["exit_code"], 0);
    let shown = output["output"]
        .as_str()
        .expect("output should decode losslessly to a string");
    assert!(shown.starts_with('a'));
    assert!(shown.ends_with('b'));
}
