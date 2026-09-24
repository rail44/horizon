//! Ordinary host execution uses async pipes and abortable output pumps.
use super::stream::pump;
#[cfg(unix)]
use super::BASH_NICE_LEVEL;
use super::{failed_output, note_undrained, status_output, take, timeout_output, wrapped_script};
use crate::config::BashToolConfig;
use crate::tools::bash::registry::Registration;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::process::Command as TokioCommand;

pub(super) async fn run_async(
    registration: &Registration,
    command: &str,
    timeout: Duration,
    drain_grace: Duration,
    cwd: &Path,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Value {
    let mut cmd = prepare_command(command, cwd);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_output(&format!("failed to start bash: {error}"), None, config);
        }
    };

    let Captured {
        outcome,
        raw_stdout,
        raw_stderr,
        drained,
    } = match capture(registration, child, timeout, drain_grace).await {
        Ok(captured) => captured,
        Err(message) => return failed_output(message, None, config),
    };
    let mut value = match outcome {
        Ok(Ok(status)) => status_output(status, raw_stdout, raw_stderr, cwd_handle, config),
        Ok(Err(wait_error)) => failed_output(
            &format!("failed to wait for bash: {wait_error}"),
            Some(raw_stdout),
            config,
        ),
        Err(_) => timeout_output(timeout, raw_stdout, config),
    };

    if !drained {
        note_undrained(&mut value, drain_grace);
    }

    value
}

fn prepare_command(command: &str, cwd: &Path) -> TokioCommand {
    let script = wrapped_script(command);
    let mut cmd = TokioCommand::new("bash");
    cmd.arg("-c")
        .arg(&script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);

    // Niceness must be set from *inside* the forked child, before it execs
    // bash -- not via a post-spawn `setpriority(PRIO_PGRP, pgid, ...)` from
    // this (the parent) process, which would race a fast-forking command
    // that spawns grandchildren before the parent gets scheduled to make
    // the call. `pre_exec` runs synchronously in the child, after `fork()`
    // and before `exec()`, so by the time bash (and therefore every
    // descendant it later forks -- nice is inherited across fork/exec)
    // starts running, its niceness is already lowered: robust regardless of
    // how quickly the command backgrounds work or spawns its own children.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            // SAFETY: `setpriority` is a thin syscall wrapper (no
            // allocation, no locking), so it's safe to call from this
            // post-fork, pre-exec context per `pre_exec`'s async-signal-
            // safety contract. `PRIO_PROCESS` + pid 0 means "the calling
            // process" -- this forked-but-not-yet-exec'd child.
            //
            // Best-effort, deliberately: a sandboxed or otherwise
            // restricted environment that denies priority changes (seen
            // under this very repo's own sandboxed dev environment) must
            // never stop the command from running at all -- niceness is a
            // hardening measure, not a correctness requirement. Failing
            // the whole spawn over it would be a far worse regression than
            // an unniced child.
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, BASH_NICE_LEVEL);
            Ok(())
        });
    }

    cmd
}

struct Captured {
    outcome: Result<std::io::Result<ExitStatus>, tokio::time::error::Elapsed>,
    raw_stdout: Vec<u8>,
    raw_stderr: Vec<u8>,
    drained: bool,
}

/// Keep cancellation registration through the bounded drain, including when
/// the child has exited but a descendant still owns an output pipe.
async fn capture(
    registration: &Registration,
    mut child: tokio::process::Child,
    timeout: Duration,
    drain_grace: Duration,
) -> Result<Captured, &'static str> {
    // `Stdio::piped()` above should guarantee both handles are `Some` --
    // but this is exactly the kind of "should never happen" spot that has
    // taken down a session's entire bash FIFO before (see the module-level
    // panic-safety notes), so treat a `None` as a harness failure to
    // report, not a panic to propagate. Reap the child (best-effort; it may
    // still be running) before bailing, so it isn't left behind.
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err("failed to start bash: stdout/stderr pipe was not available");
    };
    let stdout_buf = Arc::new(StdMutex::new(Vec::new()));
    let stderr_buf = Arc::new(StdMutex::new(Vec::new()));
    let mut stdout_task = tokio::spawn(pump(stdout, stdout_buf.clone()));
    let mut stderr_task = tokio::spawn(pump(stderr, stderr_buf.clone()));

    // Registered only once the child (and its process group, on unix) truly
    // exists, so a racing cancellation always has something real to kill.
    // `child.id()` is `None` only if the child has already been reaped,
    // which can't happen this early.
    let guard = child.id().map(|pid| registration.attach_process(pid));

    let outcome = tokio::time::timeout(timeout, child.wait()).await;
    let killed = outcome.is_err();
    if killed {
        if let Some(guard) = &guard {
            guard.kill();
        }
    }
    // Reap the child (a no-op if `wait` above already completed it). In the
    // common case that closes the pipes' write ends and the pump tasks see
    // EOF immediately.
    let _ = child.wait().await;

    // Bounded drain: the child being dead does NOT guarantee EOF on the
    // pipes — a background process it left behind still holds the write
    // ends (`some-server &`). The tree-walk kill (`kill_process_tree`)
    // now reaches `setsid`/`setpgid` escapees too, but a process forked
    // during the `/proc` snapshot window is still possible, so an
    // unbounded join here would hang the call forever past the point
    // where cancellation can help. On expiry, abort the pumps and return
    // with whatever the buffers hold — safe to read immediately afterwards:
    // this is a current-thread runtime, so an aborted pump can't be
    // concurrently touching its buffer once this `await` returns.
    let drained = tokio::time::timeout(drain_grace, async {
        let _ = tokio::join!(&mut stdout_task, &mut stderr_task);
    })
    .await
    .is_ok();
    if !drained {
        stdout_task.abort();
        stderr_task.abort();
    }

    // The registration is held through the drain deliberately: if the drain
    // is what's keeping the call alive, a user cancellation arriving in that
    // window still has a process group to SIGKILL — killing the lingering
    // pipe-holder both unblocks the drain early and honours the cancel. No
    // pid-reuse hazard in the window: the kernel can't recycle the child's
    // pid while it's still the pgid of a live group member, and once no
    // member remains the drain completes and this drops right away. Either
    // way the window is bounded by `drain_grace`.
    drop(guard);

    let raw_stdout = take(&stdout_buf);
    let raw_stderr = take(&stderr_buf);

    Ok(Captured {
        outcome,
        raw_stdout,
        raw_stderr,
        drained,
    })
}
