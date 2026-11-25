//! One execution path for every call: async pipes, abortable output pumps,
//! a process group of its own. A contained call is the sandbox runtime's
//! command around the same script.
#[cfg(unix)]
use super::BASH_NICE_LEVEL;
use super::stream::pump;
use super::{
    Execution, failed_output, infrastructure_output, note_undrained, status_output, take,
    timeout_output, wrapped_script,
};
use crate::config::BashToolConfig;
use crate::tools::bash::registry::Registration;
use crate::tools::output::Response;
use crate::tools::output::{annotate_refused_hosts, annotate_sandboxed};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::process::Command as TokioCommand;

#[expect(clippy::too_many_arguments)]
pub(super) async fn run_async(
    registration: &Registration,
    command: &str,
    timeout: Duration,
    drain_grace: Duration,
    cwd: &Path,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
    execution: &Execution,
) -> Response {
    let script = wrapped_script(command);
    let (mut cmd, _keep_alive) = match execution {
        Execution::Host => {
            let mut cmd = TokioCommand::new("bash");
            cmd.arg("-c").arg(&script);
            (cmd, Vec::new())
        }
        Execution::Contained {
            runtime,
            containment,
        } => match scratch_dir().and_then(|scratch| {
            runtime
                .contain(&script, containment, &scratch)
                .map_err(|error| error.to_string())
        }) {
            Ok(contained) => (TokioCommand::from(contained.command), contained.keep_alive),
            Err(error) => {
                return contained_failure(
                    &format!("the sandbox could not prepare this call: {error}"),
                    config,
                );
            }
        },
    };
    prepare_command(&mut cmd, cwd);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) if matches!(execution, Execution::Contained { .. }) => {
            return contained_failure(&format!("the sandbox did not start: {error}"), config);
        }
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
    let raw_stderr_copy = raw_stderr.clone();
    let mut value = match outcome {
        Ok(Ok(status)) => status_output(status, raw_stdout, raw_stderr, cwd_handle, config),
        Ok(Err(wait_error)) => failed_output(
            &format!("failed to wait for bash: {wait_error}"),
            Some(raw_stdout),
            config,
        ),
        Err(_) => timeout_output(timeout, raw_stdout, &raw_stderr, cwd_handle, config),
    };

    if !drained {
        note_undrained(&mut value, drain_grace);
    }
    annotate_sandboxed(&mut value, matches!(execution, Execution::Contained { .. }));
    if let Execution::Contained { runtime, .. } = execution {
        annotate_refused_hosts(
            &mut value,
            runtime.refused_hosts(&String::from_utf8_lossy(&raw_stderr_copy)),
        );
    }

    value
}

/// A host directory for the runtime's own files, which no contained call
/// may write: under the host's temp directory, which a contained call does
/// not see as writable.
fn scratch_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("horizon-sandbox");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

/// The sandbox could not run the call: it did not run, and the same command
/// will not fare better. The next turn probes the runtime again.
fn contained_failure(reason: &str, config: &BashToolConfig) -> Response {
    crate::tools::sandbox::reprobe();
    let mut output = infrastructure_output(
        &format!(
            "{reason}. The command did not run. Do not retry it unchanged; filesystem \
             tools remain available."
        ),
        None,
        config,
    );
    annotate_sandboxed(&mut output, true);
    output
}

fn prepare_command(cmd: &mut TokioCommand, cwd: &Path) {
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    // A parent Git process (a hook that runs this, for one) can export
    // repository-routing variables; they must not send the command to
    // another repository than the workspace's.
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_DIR",
        "GIT_GRAFT_FILE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_INTERNAL_SUPER_PREFIX",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_OBJECT_DIRECTORY",
        "GIT_PREFIX",
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_WORK_TREE",
    ] {
        cmd.env_remove(key);
    }
    // Read-only Git commands such as `status` would otherwise refresh the
    // index on the side.
    cmd.env("GIT_OPTIONAL_LOCKS", "0");

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
    if killed && let Some(guard) = &guard {
        guard.kill();
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
