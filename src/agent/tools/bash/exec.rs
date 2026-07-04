use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

use crate::agent::config::BashToolConfig;
use crate::agent::contract::ToolCallId;

use super::output::{self, Capped};
use super::registry::RegistryGuard;

/// Runs one bash call to completion (or until it times out / fails to
/// spawn), synchronously from the caller's point of view. Called on a
/// dedicated background thread (see `bash::spawn`) — never on the UI
/// thread, since a command may legitimately run for the whole timeout.
/// `config` carries the timeout/output-cap/drain-grace knobs (`[agent]` in
/// the config file, `agent::config::BashToolConfig`; see its fields'
/// doc comments for the constants they replaced).
pub(super) fn run(
    call_id: &ToolCallId,
    input: &Value,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Value {
    run_inner(
        call_id,
        input,
        cwd_handle,
        Duration::from_secs(config.drain_grace_secs),
        config,
    )
}

/// Test hook: `run` with a shortened post-exit drain bound, so tests of the
/// background-process-holds-the-pipe path don't have to sit out the full
/// production grace.
#[cfg(test)]
pub(super) fn run_with_drain_grace(
    call_id: &ToolCallId,
    input: &Value,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    drain_grace: Duration,
    config: &BashToolConfig,
) -> Value {
    run_inner(call_id, input, cwd_handle, drain_grace, config)
}

fn run_inner(
    call_id: &ToolCallId,
    input: &Value,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    drain_grace: Duration,
    config: &BashToolConfig,
) -> Value {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return error_output("bash requires a `command` string argument", None, config);
    };
    if command.trim().is_empty() {
        return error_output("bash requires a non-empty `command` string", None, config);
    }

    let timeout = resolve_timeout(input, config);
    let cwd = cwd_handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return error_output(
            "failed to start bash: could not create an async runtime",
            None,
            config,
        );
    };

    runtime.block_on(run_async(
        call_id,
        command,
        timeout,
        drain_grace,
        &cwd,
        cwd_handle,
        config,
    ))
}

fn resolve_timeout(input: &Value, config: &BashToolConfig) -> Duration {
    let secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(config.timeout_default_secs);
    Duration::from_secs(secs.clamp(1, config.timeout_max_secs))
}

async fn run_async(
    call_id: &ToolCallId,
    command: &str,
    timeout: Duration,
    drain_grace: Duration,
    cwd: &Path,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Value {
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

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return error_output(&format!("failed to start bash: {error}"), None, config);
        }
    };

    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    let stdout_buf = Arc::new(StdMutex::new(Vec::new()));
    let stderr_buf = Arc::new(StdMutex::new(Vec::new()));
    let mut stdout_task = tokio::spawn(pump(stdout, stdout_buf.clone()));
    let mut stderr_task = tokio::spawn(pump(stderr, stderr_buf.clone()));

    // Registered only once the child (and its process group, on unix) truly
    // exists, so a racing cancellation always has something real to kill.
    // `child.id()` is `None` only if the child has already been reaped,
    // which can't happen this early.
    let guard = child
        .id()
        .map(|pid| RegistryGuard::new(call_id.clone(), pid));

    let outcome = tokio::time::timeout(timeout, child.wait()).await;
    let killed = outcome.is_err();
    if killed {
        super::registry::kill(call_id);
    }
    // Reap the child (a no-op if `wait` above already completed it). In the
    // common case that closes the pipes' write ends and the pump tasks see
    // EOF immediately.
    let _ = child.wait().await;

    // Bounded drain: the child being dead does NOT guarantee EOF on the
    // pipes — a background process it left behind still holds the write
    // ends (`some-server &`; or, on the timeout path, a `setsid` grandchild
    // that escaped the process-group SIGKILL). An unbounded join here would
    // hang the call forever, past the point where cancellation can help. On
    // expiry, abort the pumps and return with whatever the buffers hold —
    // safe to read immediately afterwards: this is a current-thread
    // runtime, so an aborted pump can't be concurrently touching its
    // buffer once this `await` returns.
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

    let mut value = if killed {
        timeout_output(timeout, raw_stdout, config)
    } else {
        match outcome {
            // `status.code()` is `None` on unix when the process was
            // terminated by a signal rather than exiting on its own — which
            // is exactly what an *external* kill looks like (cancellation
            // racing this call via `bash::kill_if_running`, arriving before
            // our own timeout). Our own timeout-triggered kill is already
            // handled above via `killed`; this covers every other way the
            // child can end up signalled.
            Ok(Ok(status)) if status.code().is_some() => {
                success_output(status, raw_stdout, raw_stderr, cwd_handle, config)
            }
            Ok(Ok(status)) => terminated_output(status, raw_stdout, config),
            Ok(Err(wait_error)) => error_output(
                &format!("failed to wait for bash: {wait_error}"),
                Some(raw_stdout),
                config,
            ),
            Err(_) => unreachable!("timeout path already handled above"),
        }
    };

    if !drained {
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "note".to_string(),
                Value::String(format!(
                    "output capture stopped {}ms after the command ended: a background \
                     process is still holding the output pipe, so anything it prints later \
                     is not included",
                    drain_grace.as_millis()
                )),
            );
        }
    }

    value
}

/// Reads a child's stream to EOF, appending every chunk to `buf` as it
/// arrives. Running two of these concurrently (stdout + stderr) means a
/// timeout can still report whatever had already been produced, since `buf`
/// lives outside the timed-out future.
async fn pump(mut reader: impl tokio::io::AsyncRead + Unpin, buf: Arc<StdMutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(&chunk[..n]),
        }
    }
}

fn take(buf: &Arc<StdMutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *buf.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// Wraps the user's command so that:
/// - its own stdout and stderr are merged (`2>&1`) into the single stream
///   Horizon captures as "the" output — a real, kernel-ordered merge, not a
///   best-effort reassembly of two separately-read pipes;
/// - the final `$PWD` is reported on a *different* fd (the wrapper script's
///   own stderr) after the command finishes, so cwd tracking never has to
///   be stripped out of the shown output — it's never mixed in to begin
///   with;
/// - the command's real exit code is preserved as the wrapper's own exit
///   code.
fn wrapped_script(command: &str) -> String {
    format!(
        "{{ {command}\n}} 2>&1\n__horizon_bash_status=$?\nprintf '%s' \"$PWD\" 1>&2\nexit \"$__horizon_bash_status\"\n"
    )
}

fn success_output(
    status: ExitStatus,
    raw_stdout: Vec<u8>,
    raw_stderr: Vec<u8>,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Value {
    let mut shown_source = String::from_utf8_lossy(&raw_stdout).into_owned();
    apply_cwd_report(&raw_stderr, cwd_handle, &mut shown_source);

    let Capped { shown, truncated } = output::cap(&shown_source, config.output_cap_chars);
    let output_file = output::spill(&shown_source);

    json!({
        "exit_code": status.code(),
        "output": shown,
        "truncated": truncated,
        "output_file": output_file.map(|path| path.display().to_string()),
    })
}

/// Applies the wrapper's cwd report (see `wrapped_script`) to `cwd_handle`
/// if it looks like an absolute path, updating the session's tracked bash
/// cwd for the next call. If it doesn't — the wrapper script itself failed
/// before reaching the `printf` (e.g. a bash parse error) — the tracked cwd
/// is left unchanged and the stray text is folded into the shown output
/// instead of silently dropped.
fn apply_cwd_report(
    raw_stderr: &[u8],
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    shown_source: &mut String,
) {
    let reported = String::from_utf8_lossy(raw_stderr);
    let trimmed = reported.trim();
    if trimmed.is_empty() {
        return;
    }
    if Path::new(trimmed).is_absolute() {
        *cwd_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = PathBuf::from(trimmed);
        return;
    }
    if !shown_source.is_empty() {
        shown_source.push('\n');
    }
    shown_source.push_str(trimmed);
}

fn timeout_output(timeout: Duration, raw_stdout: Vec<u8>, config: &BashToolConfig) -> Value {
    error_output(
        &format!(
            "bash command timed out after {}s and was killed",
            timeout.as_secs()
        ),
        Some(raw_stdout),
        config,
    )
}

/// The child ended without an exit code of its own — on unix, that means a
/// signal terminated it (see the call site). This is a harness failure
/// (`is_error`), not a normal result: nothing else outside `timeout_output`
/// intentionally sends a process a fatal signal (see `bash::kill_if_running`
/// and the process-group kill it performs, called for a still-running
/// call whose turn was cancelled).
fn terminated_output(status: ExitStatus, raw_stdout: Vec<u8>, config: &BashToolConfig) -> Value {
    error_output(
        &format!("bash command was terminated{}", signal_suffix(status)),
        Some(raw_stdout),
        config,
    )
}

#[cfg(unix)]
fn signal_suffix(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(signal) => format!(" by signal {signal}"),
        None => String::new(),
    }
}

#[cfg(not(unix))]
fn signal_suffix(_status: ExitStatus) -> String {
    String::new()
}

fn error_output(message: &str, partial_output: Option<Vec<u8>>, config: &BashToolConfig) -> Value {
    match partial_output {
        None => json!({ "is_error": true, "message": message }),
        Some(raw) => {
            let source = String::from_utf8_lossy(&raw).into_owned();
            let Capped { shown, truncated } = output::cap(&source, config.output_cap_chars);
            let output_file = output::spill(&source);
            json!({
                "is_error": true,
                "message": message,
                "output": shown,
                "truncated": truncated,
                "output_file": output_file.map(|path| path.display().to_string()),
            })
        }
    }
}
