//! Bash entry points and shared output formatting. Process ownership lives
//! in plain/sandboxed execution, with each path retaining its own kill/drain rules.
mod plain;
mod sandboxed;
mod stream;
use plain::run_async;
pub(super) use sandboxed::run_sandboxed;

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use crate::tools::output::{BashOutput, BashTermination, Response};

use crate::config::BashToolConfig;
use crate::contract::ToolCallIdentity;

use super::output::{self, Capped};
use super::registry::Registration;
type BashCompletion = crate::tools::ToolCompletion<Response>;

/// Niceness applied to every spawned bash child (`docs/agent-tools-design.md`,
/// "Bash Containment"). An agent-driven command must not contend with
/// Horizon's own UI thread, or the machine owner's foreground work, for CPU
/// time -- 10 is a conventional "background priority" level: felt, but not
/// the maximum (19), since a bash call is work the agent is actively
/// waiting on, not a fire-and-forget batch job.
#[cfg(unix)]
pub(super) const BASH_NICE_LEVEL: i32 = 10;

/// Runs one bash call to completion (or until it times out / fails to
/// spawn), synchronously from the caller's point of view. Called on a
/// dedicated background thread (see `bash::spawn`) — never on the UI
/// thread, since a command may legitimately run for the whole timeout.
/// `config` carries the timeout/output-cap/drain-grace knobs (`[agent]` in
/// the config file, `agent::config::BashToolConfig`; see its fields'
/// doc comments for the constants they replaced).
pub(super) fn run(
    registration: &Registration,
    input: &crate::tools::input::Bash,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Response {
    run_inner(
        registration,
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
    registration: &Registration,
    input: &crate::tools::input::Bash,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    drain_grace: Duration,
    config: &BashToolConfig,
) -> Response {
    run_inner(registration, input, cwd_handle, drain_grace, config)
}

fn run_inner(
    registration: &Registration,
    input: &crate::tools::input::Bash,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    drain_grace: Duration,
    config: &BashToolConfig,
) -> Response {
    let command = &*input.command;

    let timeout = Duration::from_secs(input.timeout_secs.get());
    let cwd = cwd_handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return failed_output(
            "failed to start bash: could not create an async runtime",
            None,
            config,
        );
    };

    runtime.block_on(run_async(
        registration,
        command,
        timeout,
        drain_grace,
        &cwd,
        cwd_handle,
        config,
    ))
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
) -> Response {
    let mut shown_source = String::from_utf8_lossy(&raw_stdout).into_owned();
    apply_cwd_report(&raw_stderr, cwd_handle, &mut shown_source);

    let output_file = output::spill(&shown_source);
    let Capped { shown, truncated } = output::cap(
        &shown_source,
        config.output_cap_chars,
        output_file.as_deref(),
    );

    Response::succeeded(BashOutput {
        termination: BashTermination::Exited {
            exit_code: status.code().expect("ordinary exit"),
        },
        message: None,
        output: shown,
        truncated,
        output_file: output_file.map(|path| path.display().to_string()),
        note: None,
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

fn timeout_output(timeout: Duration, raw_stdout: Vec<u8>, config: &BashToolConfig) -> Response {
    let mut output = failed_output(
        &format!(
            "bash command timed out after {}s and was killed",
            timeout.as_secs()
        ),
        Some(raw_stdout),
        config,
    );
    output.bash_mut().expect("bash failure").termination = BashTermination::TimedOut {
        timeout_secs: timeout.as_secs(),
    };
    output
}

/// The child ended without an exit code of its own — on unix, that means a
/// signal terminated it (see the call site). This is a harness failure
/// (`is_error`), not a normal result: nothing else outside `timeout_output`
/// intentionally sends a process a fatal signal (see `bash::cancel_call`
/// and the process-group kill it performs, called for a still-running
/// call whose turn was cancelled).
fn terminated_output(status: ExitStatus, raw_stdout: Vec<u8>, config: &BashToolConfig) -> Response {
    let mut output = failed_output(
        &format!("bash command was terminated{}", signal_suffix(status)),
        Some(raw_stdout),
        config,
    );
    output.bash_mut().expect("bash failure").termination = BashTermination::Terminated;
    output
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

/// The worker's unwind boundary has no captured output or running process.
pub(super) fn panic_output(message: &str) -> Response {
    Response::failed(failure_body(message))
}

fn failure_body(message: &str) -> BashOutput {
    BashOutput {
        termination: BashTermination::Failed,
        message: Some(message.into()),
        output: String::new(),
        truncated: false,
        output_file: None,
        note: None,
    }
}

fn failed_output(
    message: &str,
    partial_output: Option<Vec<u8>>,
    config: &BashToolConfig,
) -> Response {
    let mut body = failure_body(message);
    if let Some(raw) = partial_output {
        let source = String::from_utf8_lossy(&raw).into_owned();
        let output_file = output::spill(&source);
        let Capped { shown, truncated } =
            output::cap(&source, config.output_cap_chars, output_file.as_deref());
        body.output = shown;
        body.truncated = truncated;
        body.output_file = output_file.map(|path| path.display().to_string());
    }
    Response::failed(body)
}

/// Build the result before classifying containment denials. A signal has no
/// cwd report: only an ordinary exit can update the session's working directory.
fn status_output(
    status: ExitStatus,
    raw_stdout: Vec<u8>,
    raw_stderr: Vec<u8>,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Response {
    match status.code() {
        Some(_) => success_output(status, raw_stdout, raw_stderr, cwd_handle, config),
        // No exit code at all means signal-terminated -- this crate's own
        // seccomp filter denies via `Errno`, not `Trap` (see
        // `horizon_sandbox::linux::seccomp`'s module doc), so a genuine
        // denial from *our* containment never lands here; treated as an
        // ordinary harness failure, same as the unsandboxed path's
        // `terminated_output`.
        None => terminated_output(status, raw_stdout, config),
    }
}

fn domain_denied(
    _identity: &ToolCallIdentity,
    domains: Vec<String>,
    output: Response,
) -> BashCompletion {
    BashCompletion::DomainDenied {
        domains,
        result: output,
    }
}

fn finished(_identity: &ToolCallIdentity, output: Response) -> BashCompletion {
    BashCompletion::Finished(output)
}

fn note_undrained(value: &mut Response, drain_grace: Duration) {
    if let Some(output) = value.bash_mut() {
        output.note = Some(format!("output capture stopped {}ms after the command ended: a background process is still holding the output pipe, so anything it prints later is not included", drain_grace.as_millis()));
    }
}
