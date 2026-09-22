//! Own the child through exit and output drain, then collect denial evidence.
use super::super::{failed_output, take};
use crate::config::BashToolConfig;
use crate::contract::ToolCallId;
#[cfg(target_os = "linux")]
use crate::policy::annotate_sandboxed;
use crate::tools::bash::registry::RegistryGuard;
use serde_json::Value;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

pub(super) struct Captured {
    pub(super) status: Option<ExitStatus>,
    pub(super) killed: bool,
    pub(super) drained: bool,
    pub(super) raw_stdout: Vec<u8>,
    pub(super) raw_stderr: Vec<u8>,
    pub(super) denials: horizon_sandbox::ContainmentDenials,
    #[cfg(target_os = "macos")]
    pub(super) denial_collection_error: Option<String>,
}

pub(super) fn collect(
    sandboxed: horizon_sandbox::SandboxedChild,
    call_id: &ToolCallId,
    timeout: Duration,
    config: &BashToolConfig,
    #[cfg(target_os = "macos")] started_at: std::time::SystemTime,
) -> Result<Captured, Value> {
    // The binding is only mutated on Linux (`supervisor_report.take()`
    // below), so the `mut` lives on a cfg'd re-bind and macOS stays
    // warning-free under `-D warnings`.
    #[cfg(target_os = "linux")]
    let mut sandboxed = sandboxed;
    #[cfg(target_os = "linux")]
    let supervisor_report = sandboxed
        .supervisor_report
        .take()
        .map(|report| std::thread::spawn(move || report.containment_denials()));
    let mut child = sandboxed.child;
    #[cfg(target_os = "macos")]
    let denial_collector = horizon_sandbox::DenialCollector::start(child.id(), started_at);

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_pid(child.id());
        let _ = child.wait();
        return Err(failed_output(
            "failed to start bash: stdout/stderr pipe was not available",
            None,
            config,
        ));
    };

    let (stdout_buf, stdout_handle) = spawn_blocking_pump(stdout);
    let (stderr_buf, stderr_handle) = spawn_blocking_pump(stderr);

    // Registered only once the child truly exists (mirroring `run_async`'s
    // own comment) so a racing cancellation always has something real to
    // kill. Unlike tokio's `Child::id()` (`Option<u32>`, `None` once
    // already reaped), `std::process::Child::id()` is plain `u32` -- always
    // available up to this point.
    let guard = RegistryGuard::new(call_id.clone(), child.id());

    let (status, killed) = wait_child_with_timeout(child, timeout);

    // Bounded drain, same rationale as `run_async`'s: a background process
    // the command left running can still hold the pipes open well past the
    // command's own exit.
    let drain_grace = Duration::from_secs(config.drain_grace_secs);
    let drained = join_within(vec![stdout_handle, stderr_handle], drain_grace);
    drop(guard);

    let raw_stdout = take(&stdout_buf);
    let raw_stderr = take(&stderr_buf);

    #[cfg(target_os = "linux")]
    let containment_denials = if killed {
        horizon_sandbox::ContainmentDenials::default()
    } else {
        match supervisor_report {
            Some(handle) => match handle.join() {
                Ok(Ok(denials)) => denials,
                Ok(Err(error)) => {
                    let mut value = failed_output(
                        &format!("sandbox supervisor report failed: {error}"),
                        Some(raw_stdout),
                        config,
                    );
                    annotate_sandboxed(&mut value, true);
                    return Err(value);
                }
                Err(_) => {
                    let mut value = failed_output(
                        "sandbox supervisor report reader panicked",
                        Some(raw_stdout),
                        config,
                    );
                    annotate_sandboxed(&mut value, true);
                    return Err(value);
                }
            },
            None => {
                let mut value = failed_output(
                    "sandbox supervisor did not provide a structured report channel",
                    Some(raw_stdout),
                    config,
                );
                annotate_sandboxed(&mut value, true);
                return Err(value);
            }
        }
    };
    // macOS: recover the run's containment denials from the kernel's
    // unified-log records -- seatbelt denies silently, so this is how the
    // deny -> approve -> sandboxed-rerun loop gets its evidence
    // (`horizon_sandbox::DenialCollector`,
    // `docs/macos-containment-denial-reporting-design.md`). Collected only
    // when the call did not succeed: a successful outcome needs no approval
    // candidate, and the query would put a fixed log-lookup cost on every
    // call. Killed/timeout runs keep Linux's empty-denials semantics, and a
    // collector failure soft-degrades into an audit annotation rather than
    // failing the call -- the report is evidence, not the boundary.
    #[cfg(target_os = "macos")]
    let (containment_denials, denial_collection_error) =
        if killed || status.as_ref().is_some_and(|status| status.success()) {
            (horizon_sandbox::ContainmentDenials::default(), None)
        } else {
            match denial_collector.collect() {
                Ok(denials) => (denials, None),
                Err(error) => (
                    horizon_sandbox::ContainmentDenials::default(),
                    Some(error.to_string()),
                ),
            }
        };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let containment_denials = horizon_sandbox::ContainmentDenials::default();
    Ok(Captured {
        status,
        killed,
        drained,
        raw_stdout,
        raw_stderr,
        denials: containment_denials,
        #[cfg(target_os = "macos")]
        denial_collection_error,
    })
}

/// Spawns a background OS thread that blocking-reads `reader` to EOF,
/// appending every chunk into a shared buffer -- the synchronous analogue
/// of `plain::pump` (which is async, for the unsandboxed tokio path). A
/// `std::process::Child`'s piped stdio has no async wrapper available in
/// this crate (`horizon_sandbox::spawn` returns a plain `std::process::
/// Child`, not a tokio one -- see that crate's doc), so this reads
/// synchronously on its own thread instead. Returns the shared buffer and
/// the join handle, so the caller can bound how long it waits for a
/// straggler (see `join_within`) without blocking this thread past that
/// bound.
fn spawn_blocking_pump(
    mut reader: impl std::io::Read + Send + 'static,
) -> (Arc<StdMutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(StdMutex::new(Vec::new()));
    let buf_for_thread = buf.clone();
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_for_thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    (buf, handle)
}

/// Waits up to `timeout` for every handle in `handles` to finish, without
/// blocking past it -- the synchronous-thread analogue of `run_async`'s
/// `tokio::time::timeout` drain bound (a background process a command left
/// running can still hold the write end of its pipe well after the command
/// itself exited; an unbounded join here would hang the call forever past
/// the point cancellation can help). Returns whether every handle actually
/// finished in time. `JoinHandle` has no timed join, so this polls
/// `is_finished`, sleeping briefly between checks -- fine here since this
/// path is already a dedicated background bash-call thread, never the UI
/// thread. A handle that didn't finish in time is simply left running,
/// detached (std threads can't be aborted): whatever it later appends to
/// its buffer is never read again, since this call's caller reads the
/// buffer's contents immediately after and moves on.
fn join_within(handles: Vec<std::thread::JoinHandle<()>>, timeout: Duration) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    let deadline = std::time::Instant::now() + timeout;
    let mut all_finished = true;
    for handle in handles {
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            all_finished = false;
        }
    }
    all_finished
}

/// Waits for `child` to finish, without blocking past `timeout`: a
/// background thread runs the actual (blocking) `child.wait()` and reports
/// back over a channel, so this can bound the wait with `recv_timeout`
/// rather than blocking on `wait()` directly. On expiry, kills `child` by
/// pid and blocks (unbounded -- killing should make `wait()` return
/// promptly) for the final status. Returns `(status, killed)`; `status` is
/// `None` only if waiting on the child itself failed (an OS-level error,
/// not a normal exit/signal outcome).
fn wait_child_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> (Option<ExitStatus>, bool) {
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let status = child.wait().ok();
        let _ = tx.send(status);
    });

    match rx.recv_timeout(timeout) {
        Ok(status) => (status, false),
        Err(_) => {
            kill_pid(pid);
            let status = rx.recv().unwrap_or(None);
            (status, true)
        }
    }
}

#[cfg(unix)]
fn kill_pid(pid: u32) {
    // Kill the entire process tree, not just the process group: the
    // sandbox child (the dedicated helper on Linux) is the process-group
    // leader, and every descendant that stays in the group is reached by
    // the group signal. But a descendant that called `setsid`/`setpgid`
    // escaped the group and survives it — `kill_process_tree` walks
    // `/proc` to find and kill those individually (issue 017).
    crate::tools::bash::registry::kill_process_tree(pid);
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {}
