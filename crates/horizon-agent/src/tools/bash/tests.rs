use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::BashToolConfig;
use crate::contract::{SessionId, ToolCallId, ToolCallResult};
use crate::frame::{AgentFrame, AgentFrameItem};

use super::BashCompletion;

fn cwd_handle(path: PathBuf) -> Arc<Mutex<PathBuf>> {
    Arc::new(Mutex::new(path))
}

fn config() -> BashToolConfig {
    BashToolConfig::default()
}

/// Unwraps a completion expected to be finished (the overwhelming majority
/// of this module's tests, which exercise the plain unsandboxed path that
/// never produces a structured containment prompt) -- panics with a useful
/// message otherwise, rather than every call site pattern-matching by hand.
fn expect_finished(completion: BashCompletion) -> ToolCallResult {
    match completion {
        BashCompletion::Finished(result) => result,
        BashCompletion::DomainDenied {
            call_id, domains, ..
        } => panic!(
            "expected a finished bash completion, got a domain-denied request for \
             {call_id:?} ({domains:?})"
        ),
        BashCompletion::FilesystemDenied {
            call_id, denials, ..
        } => panic!(
            "expected a finished bash completion, got a filesystem-denied request for \
             {call_id:?} ({denials:?})"
        ),
    }
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

// --- spawn failure ---------------------------------------------------------

/// Backlog 46 (the 2026-07-19 event-log analysis of session `2f3668b8`):
/// a single call spawned 134 real bash children in ~30s before finally
/// failing with "Too many open files (os error 24)" -- but that storm's
/// root cause was never a retry loop inside `exec::run` (there isn't one:
/// [`super::exec::run_async`]'s `cmd.spawn()` fails exactly once and
/// returns immediately, no loop). It was 134 duplicate
/// `Command::ApproveToolCall`s for the same still-running call reaching
/// `tools::approval::try_execute` before its `has_tool_call_started` guard
/// existed, each one spawning its own concurrent, never-reaped child (see
/// `resolve_approval_second_approve_of_a_still_running_bash_call_is_noop`
/// and `approved_bash_calls_for_the_same_session_are_serialized`, which
/// together close both halves of that gap: an already-started call can't
/// be approved twice, and even multiple enqueued calls for one session
/// never run concurrently).
///
/// This test covers the other half backlog 46 asked about: whether a
/// *failed* spawn attempt itself leaks anything. A nonexistent `current_dir`
/// deterministically fails `Command::spawn()` before any child exists (see
/// `run_async`'s own `cmd.spawn()` match) -- a real, non-flaky spawn-failure
/// injection point already present in the code, without needing to exhaust
/// real file descriptors or add a test-only seam. Repeating it many times
/// and comparing this process's own open-fd count before and after proves
/// each failed attempt cleans up completely: nothing --  not a runtime, not
/// a half-created pipe -- survives past the `Err` return.
#[cfg(target_os = "linux")]
#[test]
fn repeated_spawn_failures_do_not_leak_file_descriptors() {
    fn open_fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    let missing_dir =
        std::env::temp_dir().join(format!("horizon-bash-missing-{}", uuid::Uuid::new_v4()));
    let cwd = cwd_handle(missing_dir);
    let call_id = ToolCallId("spawn-fail-fd-check".to_string());

    let before = open_fd_count();
    for _ in 0..50 {
        let output = super::exec::run(&call_id, &json!({ "command": "echo hi" }), &cwd, &config());
        assert_eq!(output["is_error"], true);
        assert!(
            output["message"]
                .as_str()
                .expect("message")
                .contains("failed to start bash"),
            "expected a spawn-failure message, got: {output}"
        );
        assert!(
            !super::registry::is_registered(&call_id),
            "a spawn that never produced a child must never register a kill handle"
        );
    }
    let after = open_fd_count();

    assert!(
        after <= before + 5,
        "50 failed spawn attempts should not accumulate open fds: before {before}, after {after}"
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
    assert!(shown.contains("chars omitted"));
    assert!(shown.len() < 40_000);

    let spill_path = output["output_file"].as_str().expect("spill file path");
    // The truncation notice must inline the spill path itself -- not just
    // leave it in the separate `output_file` field -- so the model can act
    // on it without having to notice a second place to look (backlog #11).
    assert!(
        shown.contains(spill_path),
        "notice should inline the spill path: {shown}"
    );
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
    // Canonicalize so the comparison survives macOS's `/var` ->
    // `/private/var` symlink: the shell reports its PWD resolved, so an
    // unresolved expectation fails even though the directory never
    // changed.
    let base = base.canonicalize().expect("canonicalize test dir");
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
        SessionId::new(),
        call_id.clone(),
        json!({ "command": "true" }),
        cwd,
        config(),
        tx,
    );

    let completion = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("bash call should finish");
    assert_eq!(expect_finished(completion).call_id, call_id);
    assert!(!super::registry::is_registered(&call_id));
}

#[test]
fn kill_registry_kills_a_running_child_and_removes_its_entry() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("registry-kill".to_string());
    let (tx, rx) = crossbeam_channel::unbounded();

    super::spawn(
        SessionId::new(),
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
    assert_eq!(expect_finished(completion).output["is_error"], true);
}

// --- bash containment: per-session FIFO + niceness ------------------------

/// Two approved bash calls for the *same* session must never run
/// concurrently (`docs/agent-tools-design.md`, "Bash Containment"): the
/// second must not even start until the first has fully finished. Proven
/// without relying on timestamp precision: the first call creates a
/// sentinel file, sleeps, then removes it; the second (enqueued immediately
/// behind it, while the first is still "running") checks whether the
/// sentinel exists the moment *it* actually starts. If serialization is
/// broken, the second call starts concurrently and observes the sentinel
/// still there.
#[test]
fn approved_bash_calls_for_the_same_session_are_serialized() {
    let cwd = cwd_handle(std::env::temp_dir());
    let session_id = SessionId::new();
    let sentinel = std::env::temp_dir().join(format!("horizon-bash-fifo-{}", uuid::Uuid::new_v4()));
    let sentinel_str = sentinel.display().to_string();
    let (tx, rx) = crossbeam_channel::unbounded();

    super::spawn(
        session_id,
        ToolCallId("fifo-first".to_string()),
        json!({ "command": format!("touch '{sentinel_str}'; sleep 0.4; rm -f '{sentinel_str}'") }),
        cwd.clone(),
        config(),
        tx.clone(),
    );
    // Enqueued immediately, while the first call is still (from the FIFO's
    // point of view) running -- a broken implementation would start this
    // concurrently.
    super::spawn(
        session_id,
        ToolCallId("fifo-second".to_string()),
        json!({ "command": format!("if [ -f '{sentinel_str}' ]; then echo OVERLAP; else echo OK; fi") }),
        cwd,
        config(),
        tx,
    );

    let first = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the first call should finish");
    let second = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the second call should finish");

    // A session's bash calls run strictly one at a time, so completions
    // must also arrive in submission order.
    let first = expect_finished(first);
    let second = expect_finished(second);
    assert_eq!(first.call_id, ToolCallId("fifo-first".to_string()));
    assert_eq!(second.call_id, ToolCallId("fifo-second".to_string()));
    assert_eq!(second.output["output"], "OK\n");

    let _ = std::fs::remove_file(&sentinel);
}

/// A different session's bash calls must not be held up by an unrelated
/// session's FIFO -- serialization is per-session, not global.
#[test]
fn bash_calls_for_different_sessions_are_not_serialized_against_each_other() {
    let cwd = cwd_handle(std::env::temp_dir());
    let (tx, rx) = crossbeam_channel::unbounded();

    let started = Instant::now();
    super::spawn(
        SessionId::new(),
        ToolCallId("other-session-1".to_string()),
        json!({ "command": "sleep 0.5" }),
        cwd.clone(),
        config(),
        tx.clone(),
    );
    super::spawn(
        SessionId::new(),
        ToolCallId("other-session-2".to_string()),
        json!({ "command": "sleep 0.5" }),
        cwd,
        config(),
        tx,
    );

    let _ = rx.recv_timeout(Duration::from_secs(5)).expect("first call");
    let _ = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("second call");
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "two different sessions' 0.5s calls should overlap, not serialize to ~1s: {:?}",
        started.elapsed()
    );
}

/// Every bash child runs at lowered scheduling priority
/// (`docs/agent-tools-design.md`, "Bash Containment") -- read back via
/// `/proc/self/stat`'s `nice` field (the 19th whitespace-separated field,
/// per `man proc`) from a process the wrapped command itself spawns, so
/// this proves the niceness is actually inherited by the command, not just
/// set on some process nothing ever observes. `setpriority` inside
/// `pre_exec` is deliberately best-effort (a sandboxed environment may deny
/// it -- see that call site's doc comment, and this very repo's own
/// sandboxed dev environment, where it *is* denied), so this accepts either
/// the configured niceness or this test process's own ambient niceness
/// (what an un-niced child would inherit) -- anything else is a real bug.
#[cfg(unix)]
#[test]
fn bash_child_runs_at_the_configured_niceness_or_falls_back_gracefully() {
    let cwd = cwd_handle(std::env::temp_dir());
    let call_id = ToolCallId("nice-check".to_string());
    // SAFETY: `getpriority` is a plain syscall wrapper with no
    // preconditions; `PRIO_PROCESS` + pid 0 means "the calling process".
    let ambient_nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };

    // `ps -o nice=` works on both Linux and macOS; the previous
    // `/proc/self/stat` read was Linux-only and could never pass on
    // macOS (no procfs).
    let output = super::exec::run(
        &call_id,
        &json!({ "command": "ps -o nice= -p $$" }),
        &cwd,
        &config(),
    );

    assert_eq!(
        output["exit_code"], 0,
        "the command must still run even where niceness can't be changed: {output}"
    );
    let nice: i32 = output["output"]
        .as_str()
        .expect("output")
        .trim()
        .parse()
        .expect("the nice field should be a plain integer");
    assert!(
        nice == super::exec::BASH_NICE_LEVEL || nice == ambient_nice,
        "expected niceness {} (or this process's ambient niceness {ambient_nice}, if a \
         sandbox denies setpriority), got {nice}",
        super::exec::BASH_NICE_LEVEL
    );
}

// --- late-result idempotence --------------------------------------------

#[test]
fn should_fold_completion_is_false_once_the_call_already_has_a_finish() {
    let call_id = ToolCallId("late-result".to_string());
    let mut frame = AgentFrame::empty();

    assert!(super::should_fold_completion(&frame, &call_id));

    frame
        .items
        .push(AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            call_id.clone(),
            json!({ "cancelled": true }),
        )));

    assert!(
        !super::should_fold_completion(&frame, &call_id),
        "a late result must be discarded once the frame already has a finish for the call"
    );
}

// --- panic safety: FIFO advance-on-drop ------------------------------------

/// If a job panics, `registry::run_job`'s advance-on-drop guard must still
/// call `advance` -- otherwise the session's `running` flag never clears
/// and every later bash call for that session queues forever without ever
/// being dispatched (the "answered -- running..." wedge). Exercises
/// `registry::enqueue`/`run_job` directly, independent of `bash::spawn`'s
/// own panic-catching (`run_job_body`, tested separately below), so this
/// proves the FIFO's defense-in-depth guard on its own merits.
#[test]
fn registry_advances_past_a_panicking_job_so_the_queue_is_not_wedged() {
    let session_id = SessionId::new();
    let (tx, rx) = std::sync::mpsc::channel();

    super::registry::enqueue(session_id, Box::new(|| panic!("registry test panic")));
    // Enqueued immediately behind it. Whether this lands before or after
    // the first job's panic has already been caught by the thread runtime
    // and its guard has advanced the queue is a race this test doesn't
    // need to pin down -- either interleaving must still end with this job
    // running.
    super::registry::enqueue(
        session_id,
        Box::new(move || {
            let _ = tx.send(());
        }),
    );

    rx.recv_timeout(Duration::from_secs(5)).expect(
        "the second job must still run even though the job ahead of it in the FIFO panicked",
    );

    let mut cleaned_up = false;
    for _ in 0..100 {
        if !super::registry::is_session_queued(session_id) {
            cleaned_up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cleaned_up,
        "the session's queue entry should be removed once both jobs have finished"
    );
}

// --- panic safety: completion always delivered ------------------------------

#[test]
fn run_job_body_sends_a_completion_when_work_succeeds() {
    let call_id = ToolCallId("panic-safe-normal".to_string());
    let (tx, rx) = crossbeam_channel::unbounded();

    let work_call_id = call_id.clone();
    super::run_job_body(SessionId::new(), call_id.clone(), &tx, move || {
        BashCompletion::Finished(ToolCallResult::new(
            work_call_id.clone(),
            json!({ "ok": true }),
        ))
    });

    let completion = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("completion should be sent");
    let result = expect_finished(completion);
    assert_eq!(result.call_id, call_id);
    assert_eq!(result.output, json!({ "ok": true }));
}

/// The core guarantee: even if the work function panics (standing in for a
/// hypothetical panic inside `exec::run`, without actually triggering one),
/// a `BashCompletion` carrying an `is_error` output is still delivered --
/// this is what heals a stuck "answered -- running..." tool-call block
/// instead of leaving it wedged forever.
#[test]
fn run_job_body_still_sends_a_completion_when_work_panics() {
    let call_id = ToolCallId("panic-safe-panic".to_string());
    let (tx, rx) = crossbeam_channel::unbounded();

    super::run_job_body(SessionId::new(), call_id.clone(), &tx, || {
        panic!("injected panic, not exec::run's own")
    });

    let completion = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("a completion must still be delivered when the work function panics");
    let result = expect_finished(completion);
    assert_eq!(result.call_id, call_id);
    assert_eq!(result.output["is_error"], true);
    assert!(result.output["message"]
        .as_str()
        .expect("message")
        .contains("injected panic"));
}

// --- panic safety: exec.rs panic points --------------------------------

/// `Ord::clamp` panics if `min > max`; `resolve_timeout` clamps into
/// `1..=config.timeout_max_secs`, so a misconfigured `timeout_max_secs` of
/// 0 used to panic the bash worker thread outright. It must instead fall
/// back to a valid (>= 1s) timeout.
#[test]
fn resolve_timeout_does_not_panic_when_timeout_max_secs_is_zero() {
    let mut cfg = config();
    cfg.timeout_max_secs = 0;

    let timeout = super::exec::resolve_timeout(&json!({}), &cfg);
    assert!(timeout >= Duration::from_secs(1));

    let timeout_with_override = super::exec::resolve_timeout(&json!({ "timeout_secs": 5 }), &cfg);
    assert!(timeout_with_override >= Duration::from_secs(1));
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
