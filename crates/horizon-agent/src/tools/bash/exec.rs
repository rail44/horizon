use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;

use crate::config::BashToolConfig;
use crate::contract::{ToolCallId, ToolCallResult};
use crate::policy::{annotate_denied_domains, annotate_sandboxed};
use crate::tools::error_output;
use crate::tools::network::SessionNetworkProxy;

use super::output::{self, Capped};
use super::registry::RegistryGuard;
use super::BashCompletion;

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
        return failed_output("bash requires a `command` string argument", None, config);
    };
    if command.trim().is_empty() {
        return failed_output("bash requires a non-empty `command` string", None, config);
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
        return failed_output(
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

// `pub(super)`, not private: exercised directly from `tests.rs` so the
// zero-`timeout_max_secs` edge case (see the doc comment below) is proven
// against the function itself, not just indirectly through `run`.
pub(super) fn resolve_timeout(input: &Value, config: &BashToolConfig) -> Duration {
    let secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(config.timeout_default_secs);
    // `Ord::clamp` panics if `min > max`; guard against a misconfigured
    // `timeout_max_secs` of 0 (which would make the clamp's max less than
    // its min of 1) rather than let that panic take down the whole bash
    // FIFO for the session (see the panic-safety notes on `bash::spawn` and
    // `registry::run_job`).
    Duration::from_secs(secs.clamp(1, config.timeout_max_secs.max(1)))
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

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_output(&format!("failed to start bash: {error}"), None, config);
        }
    };

    // `Stdio::piped()` above should guarantee both handles are `Some` --
    // but this is exactly the kind of "should never happen" spot that has
    // taken down a session's entire bash FIFO before (see the module-level
    // panic-safety notes), so treat a `None` as a harness failure to
    // report, not a panic to propagate. Reap the child (best-effort; it may
    // still be running) before bailing, so it isn't left behind.
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return failed_output(
            "failed to start bash: stdout/stderr pipe was not available",
            None,
            config,
        );
    };
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
            Ok(Err(wait_error)) => failed_output(
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

    let output_file = output::spill(&shown_source);
    let Capped { shown, truncated } = output::cap(
        &shown_source,
        config.output_cap_chars,
        output_file.as_deref(),
    );

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
    failed_output(
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
    failed_output(
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

/// Delegates to the unified `tools::error_output` for the base error shape,
/// for `bash::spawn` (`mod.rs`) to use when the work function itself panics
/// (caught via `catch_unwind`, never reaching this module's normal error
/// paths at all) -- see that module's panic-safety notes.
pub(super) fn panic_output(message: &str) -> Value {
    error_output(message)
}

fn failed_output(message: &str, partial_output: Option<Vec<u8>>, config: &BashToolConfig) -> Value {
    match partial_output {
        None => error_output(message),
        Some(raw) => {
            let source = String::from_utf8_lossy(&raw).into_owned();
            let output_file = output::spill(&source);
            let Capped { shown, truncated } =
                output::cap(&source, config.output_cap_chars, output_file.as_deref());
            let mut value = error_output(message);
            if let Some(map) = value.as_object_mut() {
                map.insert("output".to_string(), Value::String(shown));
                map.insert("truncated".to_string(), Value::Bool(truncated));
                map.insert(
                    "output_file".to_string(),
                    output_file
                        .map(|path| Value::String(path.display().to_string()))
                        .unwrap_or(Value::Null),
                );
            }
            value
        }
    }
}

// --- sandboxed execution (tier 1: docs/agent-approval-design.md) ----------
//
// A tier-1-auto-approved `bash` call runs through `horizon_sandbox::spawn`
// instead of a plain `TokioCommand`. `horizon_sandbox::spawn` hands back a
// plain `std::process::Child` (there is no tokio integration in that crate
// -- see its own crate doc), so this is a fully synchronous, thread-based
// implementation rather than reusing `run_async`'s tokio machinery: a
// watcher thread bounds the wait with `timeout` (killing by pid on
// expiry -- see `wait_child_with_timeout`'s doc comment), and two more
// threads blocking-pump stdout/stderr
// into shared buffers, bounded by `drain_grace` the same way `run_async`
// bounds its own tokio pumps. This already runs on its own dedicated
// background thread (`bash::spawn_sandboxed` -> `registry::enqueue`), so
// there is no UI-thread-blocking concern in doing this synchronously.

/// Runs one *sandboxed* bash call to completion (or until it times out).
/// `workspace_root` is the base writable root; explicitly approved session
/// grants are additive. Filesystem denials come from the authenticated Linux
/// supervisor report rather than output or exit-code heuristics.
///
/// Network (`docs/agent-approval-design.md` leg 4b): `Some(network)` gets
/// `NetworkPolicy::Proxied { proxy_addr }` for that session's exact loopback
/// TCP endpoint. Standard HTTP proxy variables make ordinary clients use it;
/// the Linux supervisor independently refuses every other remote endpoint,
/// so those variables are compatibility plumbing rather than the security
/// boundary. `None` falls back to plain `NetworkPolicy::Disabled`.
///
/// A denied domain is detected proxy-side, never from the child's own exit
/// code (backlog 59: a piped command like `curl ... | head` can exit `0`
/// even though the network call itself was refused) -- right after the
/// child exits, this drains `network`'s recorded denials
/// (`SessionNetworkProxy::drain_denied_hosts`) and, if any were recorded,
/// returns [`BashCompletion::DomainDenied`] instead of a plain `Finished`
/// result, regardless of what the child's own exit status/output would
/// otherwise suggest. The proxy record is authoritative; output text is not
/// used to infer a grant.
///
/// Containment fix (2026-07-19 dogfooding, backlog): this policy used to add
/// `std::env::temp_dir()` (the *host's* real temp dir) as a second writable
/// root, on the theory that the bash tool's own output-spill file
/// (`output::spill`) and a command's ordinary scratch use both needed it.
/// That was wrong on both counts and, worse, actively dangerous: `spill` is
/// called from *this* function, on the host process, after the sandboxed
/// child has already exited and its output has been captured over a pipe --
/// it never runs inside the sandbox and needs no writable-root grant at all.
/// And adding the host's real temp dir as a writable root actively broke
/// containment: a live dogfooding session observed a tier-1 auto-approved
/// `echo ... > /tmp/<name>` writing through to the real host `/tmp` while
/// the result still carried `sandboxed: true`. The sandbox now provides a
/// private scratch dir instead (`horizon_sandbox`'s TMPDIR-parity
/// provisioning under the first writable root -- `SCRATCH_DIR_NAME` -- which
/// replaced the retired bwrap backend's private `--tmpfs /tmp`), so the host
/// temp dir must never be a writable root.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_sandboxed(
    call_id: &ToolCallId,
    input: &Value,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    workspace_root: &Path,
    network: Option<&SessionNetworkProxy>,
    loopback_connect: &[std::net::SocketAddr],
    filesystem_grants: &[horizon_sandbox::FilesystemGrant],
    config: &BashToolConfig,
) -> BashCompletion {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return finished(
            call_id,
            failed_output("bash requires a `command` string argument", None, config),
        );
    };
    if command.trim().is_empty() {
        return finished(
            call_id,
            failed_output("bash requires a non-empty `command` string", None, config),
        );
    }
    if let Some(message) = super::cargo::shared_cache_clean_refusal(command, workspace_root) {
        return finished(call_id, failed_output(message, None, config));
    }

    let timeout = resolve_timeout(input, config);
    let cwd = cwd_handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    // Same wrapper shape as the unsandboxed path (`wrapped_script`): merges
    // the command's own stdout+stderr, and reports the final `$PWD` on the
    // wrapper's own stderr afterward so cwd tracking keeps working across
    // sandboxed calls too. The command's own stderr ends up in this
    // wrapper's stdout via that merge and remains visible in the result.
    let script = wrapped_script(command);
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c").arg(&script).current_dir(&cwd);
    // A parent Git process (notably this repository's pre-commit hook) can
    // export repository-routing variables. They must not redirect the
    // sandboxed command away from the workspace whose metadata roots Horizon
    // validated and displayed for approval.
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
    // Read-only Git commands such as `status` may otherwise refresh the
    // index as a performance optimization. The metadata classifier keeps
    // those commands in tier 1, so suppress optional locks/writes while
    // leaving locks required by approved mutating operations intact.
    cmd.env("GIT_OPTIONAL_LOCKS", "0");

    let network_policy = match network.map(SessionNetworkProxy::proxy_addr) {
        Some(proxy_addr) => horizon_sandbox::NetworkPolicy::Proxied {
            proxy_addr,
            loopback_connect: loopback_connect.to_vec(),
            unix_socket_connect: unix_socket_connect_grants(
                std::env::var_os("SSH_AUTH_SOCK").as_deref(),
                std::env::var_os("HOME").as_deref().map(Path::new),
            ),
        },
        None => horizon_sandbox::NetworkPolicy::Disabled,
    };
    if let Some(network) = network {
        configure_proxy_environment(&mut cmd, &network.proxy_url());
        // macOS only: the tunnel rides BSD nc's CONNECT support; Linux's
        // enforcement layer and netcat dialect are a separate port.
        #[cfg(target_os = "macos")]
        configure_ssh_tunneling(&mut cmd, network.proxy_addr().port(), workspace_root);
    }
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![workspace_root.to_path_buf()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: network_policy,
    };

    // Harness-provisioned grants first (`~/.gnupg` for gpg signing), then
    // the caller's approved/configured grants, deduplicated by value so a
    // config or judge grant for the same path stays a single entry.
    let mut effective_grants =
        default_filesystem_grants(std::env::var_os("HOME").as_deref().map(Path::new));
    for grant in filesystem_grants {
        if !effective_grants.contains(grant) {
            effective_grants.push(grant.clone());
        }
    }

    let sandboxed = match horizon_sandbox::spawn_with_filesystem_grants(
        cmd,
        &policy,
        &effective_grants,
        horizon_sandbox::SandboxStdio::piped_output(),
    ) {
        Ok(sandboxed) => sandboxed,
        Err(error) => {
            return finished(
                call_id,
                failed_output(
                    &format!("failed to start sandboxed bash: {error}"),
                    None,
                    config,
                ),
            );
        }
    };
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

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_pid(child.id());
        let _ = child.wait();
        return finished(
            call_id,
            failed_output(
                "failed to start bash: stdout/stderr pipe was not available",
                None,
                config,
            ),
        );
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
                    return finished(call_id, value);
                }
                Err(_) => {
                    let mut value = failed_output(
                        "sandbox supervisor report reader panicked",
                        Some(raw_stdout),
                        config,
                    );
                    annotate_sandboxed(&mut value, true);
                    return finished(call_id, value);
                }
            },
            None => {
                let mut value = failed_output(
                    "sandbox supervisor did not provide a structured report channel",
                    Some(raw_stdout),
                    config,
                );
                annotate_sandboxed(&mut value, true);
                return finished(call_id, value);
            }
        }
    };
    #[cfg(not(target_os = "linux"))]
    let containment_denials = horizon_sandbox::ContainmentDenials::default();
    let filesystem_denials = containment_denials.filesystem;
    let network_denials = containment_denials.network;
    // Deliberately not an approval candidate: no grant exists that could
    // satisfy these, so the model is told what to do instead (see
    // `horizon_sandbox::UngrantableDenial`).
    let ungrantable_denials = containment_denials.ungrantable;

    // Drained once the child has fully exited, so no further request can
    // still be in flight against the proxy -- see this function's own doc
    // comment for why this is checked ahead of (and independent of) the
    // child's own exit status.
    let denied_domains = network
        .map(SessionNetworkProxy::drain_denied_hosts)
        .unwrap_or_default();

    if killed {
        let mut value = timeout_output(timeout, raw_stdout, config);
        annotate_sandboxed(&mut value, true);
        crate::policy::annotate_network_denials(&mut value, &network_denials);
        crate::policy::annotate_ungrantable_denials(&mut value, &ungrantable_denials);
        if !drained {
            note_undrained(&mut value, config);
        }
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
            return domain_denied(call_id, denied_domains, value);
        }
        return finished(call_id, value);
    }

    let Some(status) = status else {
        let mut value = failed_output(
            "failed to wait for sandboxed bash",
            Some(raw_stdout),
            config,
        );
        annotate_sandboxed(&mut value, true);
        crate::policy::annotate_network_denials(&mut value, &network_denials);
        crate::policy::annotate_ungrantable_denials(&mut value, &ungrantable_denials);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
            return domain_denied(call_id, denied_domains, value);
        }
        return finished(call_id, value);
    };

    if !filesystem_denials.is_empty() {
        let mut value = status_output(status, raw_stdout, raw_stderr, cwd_handle, config);
        annotate_sandboxed(&mut value, true);
        crate::policy::annotate_filesystem_denials(&mut value, &filesystem_denials);
        crate::policy::annotate_network_denials(&mut value, &network_denials);
        crate::policy::annotate_ungrantable_denials(&mut value, &ungrantable_denials);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
        }
        if !drained {
            note_undrained(&mut value, config);
        }
        return BashCompletion::FilesystemDenied {
            call_id: call_id.clone(),
            denials: filesystem_denials,
            // `occurrence_id` is `None` here: this is the bash executor
            // constructing the `prior_result` of an
            // `ApprovalKind::FilesystemDenialRetry` approval, and the
            // agentd's `fold_filesystem_denied` already knows the request's
            // `occurrence_id` (it pulled `original_request` off the frame)
            // and stamps it onto the final `ToolCallResult` it forwards to
            // the agent (see `crates/horizon-agentd/src/session/
            // completion.rs`'s `fold_filesystem_denied`). The transcript
            // matches results by `occurrence_id` first, so a `None` here
            // would re-collide on reused `call_id`s -- the fixup at fold
            // time is what keeps provider-reuse and denial-retry separable.
            result: ToolCallResult::new(call_id.clone(), None, value),
        };
    }

    // Authoritative regardless of the wrapped shell pipeline's own exit
    // code -- see this function's own doc comment (backlog 59). Output text
    // never names or authorizes a domain grant.
    if !denied_domains.is_empty() {
        let mut value = status_output(status, raw_stdout, raw_stderr, cwd_handle, config);
        annotate_sandboxed(&mut value, true);
        crate::policy::annotate_network_denials(&mut value, &network_denials);
        crate::policy::annotate_ungrantable_denials(&mut value, &ungrantable_denials);
        annotate_denied_domains(&mut value, &denied_domains);
        if !drained {
            note_undrained(&mut value, config);
        }
        return domain_denied(call_id, denied_domains, value);
    }

    let mut value = status_output(status, raw_stdout, raw_stderr, cwd_handle, config);
    annotate_sandboxed(&mut value, true);
    crate::policy::annotate_network_denials(&mut value, &network_denials);
    crate::policy::annotate_ungrantable_denials(&mut value, &ungrantable_denials);
    if !drained {
        note_undrained(&mut value, config);
    }
    // Same fixup-at-fold-time story as the `FilesystemDenied` arm above:
    // `fold_finished_bash_result` stamps the originating request's
    // `occurrence_id` onto the result before forwarding it.
    finished(call_id, value)
}

fn configure_proxy_environment(command: &mut std::process::Command, proxy_url: &str) {
    for key in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "CARGO_HTTP_PROXY",
    ] {
        command.env(key, proxy_url);
    }
    // An inherited bypass list would send matching hosts to a route the
    // kernel deliberately refuses. Empty values are understood by the common
    // clients and make the session proxy the only configured HTTP route.
    command.env("no_proxy", "").env("NO_PROXY", "");
    // Do not claim arbitrary-protocol proxy compatibility. The allowlist
    // proxy is HTTP/CONNECT; scheme-specific variables above cover web tools.
    command.env_remove("all_proxy").env_remove("ALL_PROXY");
}

/// macOS: route Git's SSH transport through the session proxy. The
/// Seatbelt profile's only non-DNS TCP egress is the proxy endpoint, so a
/// plain `git push` to an SSH remote dies at `connect(2)` with "Operation
/// not permitted". BSD `nc -X connect` bridges the gap: it CONNECT-tunnels
/// to `<host>:22`, which the allowlist proxy accepts for any approved host
/// regardless of port (verified live: `CONNECT github.com:22` tunnels once
/// `github.com` is approved), so domain approval rides the existing
/// `DomainDenialRetry` flow. `UserKnownHostsFile` moves into the sandbox
/// scratch dir -- the real `~/.ssh/known_hosts` is not writable here --
/// with `accept-new` TOFU semantics, and `ssh` reaches its keys through
/// the unix-socket grants on the network policy. Linux is untouched for
/// now (its enforcement layer and netcat dialect differ).
#[cfg(target_os = "macos")]
fn configure_ssh_tunneling(
    command: &mut std::process::Command,
    proxy_port: u16,
    workspace_root: &Path,
) {
    // An inherited variant would silently bypass this session's tunnel (or
    // point at another session's proxy address).
    command.env_remove("GIT_SSH_COMMAND").env_remove("GIT_SSH");
    let known_hosts = workspace_root
        .join(horizon_sandbox::SCRATCH_DIR_NAME)
        .join("known_hosts");
    // `tmpdir::provision` normally creates the scratch dir; create it here
    // too so the known_hosts file's parent exists even when the caller set
    // its own TMPDIR (which skips provisioning).
    let _ = std::fs::create_dir_all(known_hosts.parent().expect("scratch join has a parent"));
    command.env(
        "GIT_SSH_COMMAND",
        git_ssh_command_for_proxy(proxy_port, &known_hosts),
    );
}

/// The `GIT_SSH_COMMAND` value: SSH over the session proxy, with host keys
/// accumulated in the sandbox scratch dir (TOFU via `accept-new` -- no
/// interactive prompt a contained session could never answer).
// Production callers are macOS-only (`configure_ssh_tunneling`, whose own
// doc comment records Linux's separate enforcement/netcat dialect); on
// Linux the only caller is `sandbox_provisioning_tests`, which clippy's
// non-test lib target cannot see.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn git_ssh_command_for_proxy(proxy_port: u16, known_hosts: &Path) -> String {
    format!(
        "ssh -o ProxyCommand='/usr/bin/nc -X connect -x 127.0.0.1:{proxy_port} %h %p' \
         -o UserKnownHostsFile='{}' -o StrictHostKeyChecking=accept-new",
        known_hosts.display()
    )
}

/// `~/.gnupg` read-write when it exists: gpg-signed commits (this repo
/// sets `commit.gpgsign`) write lockfiles there and spawn `gpg-agent`.
/// Reads were never contained (`ReadableScope::Full` grants `/`), so this
/// only adds the write side signing needs, and only while the directory
/// exists. Caller-provided grants (config or judge-approved) are merged
/// after these and deduplicated by value at the merge site.
fn default_filesystem_grants(home: Option<&Path>) -> Vec<horizon_sandbox::FilesystemGrant> {
    let Some(home) = home else {
        return Vec::new();
    };
    let gnupg = home.join(".gnupg");
    if !gnupg.is_dir() {
        return Vec::new();
    }
    vec![horizon_sandbox::FilesystemGrant {
        path: gnupg,
        access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
        scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: Vec::new(),
    }]
}

/// Harness-provisioned unix-socket `connect` grants for a proxied sandbox
/// (`NetworkPolicy::Proxied`'s `unix_socket_connect` field): the ssh-agent
/// socket for SSH remotes, and -- when a GPG home exists -- its children
/// with a bind allowance, because the sandboxed command itself spawns the
/// gpg-agent that binds them. Without these, `git push` over SSH fails at
/// the agent connect ("Error connecting to agent: Operation not
/// permitted") and signed commits fail at keyboxd/gpg-agent -- both
/// observed live in the 2026-09-07 session this fix comes from.
fn unix_socket_connect_grants(
    ssh_auth_sock: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Vec<horizon_sandbox::UnixSocketConnectGrant> {
    let mut grants = Vec::new();
    if let Some(sock) = ssh_auth_sock {
        grants.push(horizon_sandbox::UnixSocketConnectGrant {
            path: PathBuf::from(sock),
            scope: horizon_sandbox::UnixSocketConnectScope::File,
            allow_bind: false,
        });
    }
    if let Some(home) = home {
        let gnupg = home.join(".gnupg");
        if gnupg.is_dir() {
            grants.push(horizon_sandbox::UnixSocketConnectGrant {
                path: gnupg,
                scope: horizon_sandbox::UnixSocketConnectScope::DirChildren,
                allow_bind: true,
            });
        }
    }
    grants
}

/// Builds the ordinary (non-timeout, non-wait-failure) result value from a
/// sandboxed child's exit status -- shared by the plain success path and the
/// domain-denied path above, which both need the same success/terminated
/// dispatch just to wrap a different [`BashCompletion`] shape around it.
/// Mirrors the `None` (signal-terminated) arm's existing behavior: `terminated_output`
/// takes no `raw_stderr` at all, so it is simply dropped in that arm, same
/// as before this was factored out.
fn status_output(
    status: ExitStatus,
    raw_stdout: Vec<u8>,
    raw_stderr: Vec<u8>,
    cwd_handle: &Arc<StdMutex<PathBuf>>,
    config: &BashToolConfig,
) -> Value {
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

fn domain_denied(call_id: &ToolCallId, domains: Vec<String>, output: Value) -> BashCompletion {
    BashCompletion::DomainDenied {
        call_id: call_id.clone(),
        domains,
        // `occurrence_id` is fixed up by `fold_domain_denied` at agentd
        // fold time -- see the comment on the `FilesystemDenied` arm of
        // `run_sandboxed` above.
        result: ToolCallResult::new(call_id.clone(), None, output),
    }
}

fn finished(call_id: &ToolCallId, output: Value) -> BashCompletion {
    BashCompletion::Finished(ToolCallResult::new(call_id.clone(), None, output))
}

fn note_undrained(value: &mut Value, config: &BashToolConfig) {
    if let Some(map) = value.as_object_mut() {
        map.insert(
            "note".to_string(),
            Value::String(format!(
                "output capture stopped {}ms after the command ended: a background \
                 process is still holding the output pipe, so anything it prints later \
                 is not included",
                Duration::from_secs(config.drain_grace_secs).as_millis()
            )),
        );
    }
}

/// Spawns a background OS thread that blocking-reads `reader` to EOF,
/// appending every chunk into a shared buffer -- the synchronous analogue
/// of `pump` above (which is async, for the unsandboxed tokio path). A
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
    super::registry::kill_process_tree(pid);
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {}

#[cfg(test)]
mod proxy_environment_tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::configure_proxy_environment;

    #[test]
    fn standard_http_clients_are_routed_without_configuring_unrelated_protocols() {
        let mut command = std::process::Command::new("true");
        configure_proxy_environment(&mut command, "http://127.0.0.1:43210");
        let env = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect::<BTreeMap<_, _>>();

        for key in [
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "CARGO_HTTP_PROXY",
        ] {
            assert_eq!(
                env.get(OsStr::new(key)).and_then(Option::as_deref),
                Some(OsStr::new("http://127.0.0.1:43210"))
            );
        }
        for key in ["no_proxy", "NO_PROXY"] {
            assert_eq!(
                env.get(OsStr::new(key)).and_then(Option::as_deref),
                Some(OsStr::new(""))
            );
        }
        for key in ["all_proxy", "ALL_PROXY"] {
            assert!(matches!(env.get(OsStr::new(key)), Some(None)));
        }
    }
}

#[cfg(test)]
mod sandbox_provisioning_tests {
    use std::path::{Path, PathBuf};

    use super::{default_filesystem_grants, git_ssh_command_for_proxy, unix_socket_connect_grants};

    fn tmp_home_with_gnupg(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("hzn-exec-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".gnupg")).expect("create fake home");
        home
    }

    #[test]
    fn gnupg_grant_is_provisioned_only_when_the_directory_exists() {
        let home = tmp_home_with_gnupg("gnupg-grant");
        let grants = default_filesystem_grants(Some(&home));
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path, home.join(".gnupg"));
        assert_eq!(
            grants[0].access,
            horizon_sandbox::FilesystemGrantAccess::ReadWrite
        );
        assert_eq!(
            grants[0].scope,
            horizon_sandbox::FilesystemGrantScope::DirectoryTree
        );

        let missing = home.join("no-such-gnupg");
        assert!(default_filesystem_grants(Some(&missing)).is_empty());
        assert!(default_filesystem_grants(None).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unix_socket_grants_cover_the_agent_and_the_gnupg_children() {
        let home = tmp_home_with_gnupg("unix-socket-grants");
        let grants = unix_socket_connect_grants(
            Some(std::ffi::OsStr::new(
                "/var/run/com.apple.launchd.x/Listeners",
            )),
            Some(&home),
        );
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants[0].path,
            PathBuf::from("/var/run/com.apple.launchd.x/Listeners")
        );
        assert_eq!(
            grants[0].scope,
            horizon_sandbox::UnixSocketConnectScope::File
        );
        assert!(!grants[0].allow_bind, "ssh-agent is a pure client");
        assert_eq!(grants[1].path, home.join(".gnupg"));
        assert_eq!(
            grants[1].scope,
            horizon_sandbox::UnixSocketConnectScope::DirChildren
        );
        assert!(grants[1].allow_bind, "gpg-agent binds its own sockets");

        assert_eq!(unix_socket_connect_grants(None, Some(&home)).len(), 1);
        assert!(unix_socket_connect_grants(None, None).is_empty());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_ssh_tunnel_command_targets_the_proxy_and_scratch_known_hosts() {
        let command =
            git_ssh_command_for_proxy(59279, Path::new("/ws/.horizon-sandbox-tmp/known_hosts"));
        assert!(command.starts_with("ssh "));
        assert!(
            command.contains("ProxyCommand='/usr/bin/nc -X connect -x 127.0.0.1:59279 %h %p'"),
            "unexpected tunnel command: {command}"
        );
        assert!(
            command.contains("UserKnownHostsFile='/ws/.horizon-sandbox-tmp/known_hosts'"),
            "unexpected known_hosts: {command}"
        );
        assert!(command.contains("StrictHostKeyChecking=accept-new"));
    }
}
