//! Linux sandbox backend: `nono` (Landlock-based capability sandboxing;
//! see `docs/agent-approval-design.md`'s "Sandbox architecture" for the
//! design and `crate::caps` for the `SandboxPolicy` -> `nono::CapabilitySet`
//! mapping, shared with the macOS backend). Migrated 2026-07-19 from a
//! self-built bwrap+seccompiler+landlock stack (`docs/roadmap.md`'s
//! backlog-60 entry, "option C"), de-risked in `experiments/nono-spike/`
//! (branch `worktree-agent-afb6d8b9e874320c8`, commit `533554b`).
//!
//! Production uses a dedicated single-threaded helper process. It forks the
//! target, applies nono Landlock in that child, and retains an unsandboxed
//! parent to answer seccomp notifications and publish an authenticated report.
//! `horizon-sessiond` therefore never forks or self-applies containment. The
//! direct dedicated-thread path remains only for this crate's legacy backend
//! unit tests, where no structured supervisor report is required.
//!
//! **Accepted regression (backlog 60):** nono has no mount or PID
//! namespace, unlike bwrap. A sandboxed child can see the host's full
//! process list (`ps`, `/proc/<pid>`) and mount table. Filesystem,
//! network, and (new, see `crate::caps`) signal containment are still
//! fully enforced via Landlock.

use crate::error::SandboxError;
use crate::policy::{SandboxPolicy, SandboxStdio};
use crate::SandboxedChild;
use std::ffi::OsString;
#[cfg(not(test))]
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Runs inside the dedicated Linux helper binary, never in sessiond.
///
/// Public only because Cargo's bin target is a separate crate from this
/// package's library target.
#[doc(hidden)]
pub fn execute_supervised_helper(
    helper_policy: &crate::HelperPolicy,
    program: &std::ffi::OsStr,
    args: Vec<OsString>,
    report_fd: std::os::fd::RawFd,
) -> Result<i32, SandboxError> {
    let caps =
        crate::caps::build_with_grants(&helper_policy.sandbox, &helper_policy.filesystem_grants)?;
    let mut command = Command::new(program);
    command.args(args);

    // SAFETY: the helper receives ownership of this descriptor across exec;
    // no other Rust owner for it exists in the helper process.
    let writer = unsafe { horizon_sandbox_runtime::ReportWriter::from_raw_fd(report_fd) };
    let outcome = horizon_sandbox_runtime::execute(command, caps, &[writer.as_raw_fd()])
        .map_err(SandboxError::SupervisedRuntime)?;
    let exit_code = outcome.exit_code;
    writer
        .write(outcome)
        .map_err(SandboxError::SupervisorReport)?;
    Ok(exit_code)
}

/// Authenticated structured result from the dedicated Linux helper.
#[derive(Debug)]
pub struct SupervisorReport {
    reader: horizon_sandbox_runtime::ReportReader,
    helper_pid: u32,
}

impl SupervisorReport {
    /// Receives the helper's one bounded report and verifies its sender PID.
    pub fn read(
        self,
    ) -> Result<horizon_sandbox_runtime::SupervisedOutcome, horizon_sandbox_runtime::ReportError>
    {
        self.reader.read(self.helper_pid)
    }

    /// Returns authoritative grantable filesystem attempts plus structured,
    /// non-grantable network/IPC bypass attempts.
    pub fn containment_denials(self) -> Result<crate::ContainmentDenials, SandboxError> {
        let outcome = self
            .reader
            .read(self.helper_pid)
            .map_err(SandboxError::SupervisorReport)?;
        let mut denials = Vec::new();
        for entry in outcome.approvals {
            if let nono::ApprovalRequest::Capability { path, access, .. } = entry.request {
                let access = match access {
                    nono::AccessMode::Read => crate::FilesystemGrantAccess::Read,
                    nono::AccessMode::Write | nono::AccessMode::ReadWrite => {
                        crate::FilesystemGrantAccess::ReadWrite
                    }
                };
                match crate::grant::resolve_denial(path, access) {
                    Ok(denial) if !denials.contains(&denial) => denials.push(denial),
                    Ok(_) | Err(SandboxError::UnsupportedGrantTarget(_)) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let mut network = Vec::new();
        for record in outcome.ipc_denials {
            let denial = crate::NetworkDenial {
                target: record.target,
                operation: record.operation,
                reason: record.reason,
            };
            if !network.contains(&denial) {
                network.push(denial);
            }
        }
        Ok(crate::ContainmentDenials {
            filesystem: denials,
            network,
        })
    }
}

/// Cheap capability probe backing [`crate::is_available`]: whether
/// Landlock is available on this kernel at all (nono's own
/// `Sandbox::detect_abi`, internally cached after the first call), with
/// no process spawned and no capability set built.
pub(crate) fn is_available() -> bool {
    nono::Sandbox::detect_abi().is_ok()
}

/// Test-only direct spawn retained for the backend's low-level unit tests.
#[cfg(test)]
pub(crate) fn spawn_with_grants(
    command: Command,
    policy: &SandboxPolicy,
    filesystem_grants: &[crate::FilesystemGrant],
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    let abi = nono::Sandbox::detect_abi()?;
    let caps = crate::caps::build_with_grants(policy, filesystem_grants)?;

    let program = command.get_program().to_os_string();
    let args: Vec<OsString> = command.get_args().map(|a| a.to_os_string()).collect();

    let mut wrapped = Command::new(&program);
    wrapped.args(&args);
    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(v) => {
                wrapped.env(key, v);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }

    // TMPDIR parity (`docs/roadmap.md`'s backlog-60 entry): bwrap gave a
    // private tmpfs `/tmp` for free via its mount namespace; nono has no
    // mount namespace at all, so there is nothing to substitute a fresh
    // `/tmp` with. Hoisted to `crate::tmpdir` since macOS needs the exact
    // same substitution -- see that module's doc.
    crate::tmpdir::provision(policy, &command, &mut wrapped)?;

    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);

    // `CapabilitySet`, `DetectedAbi`, and `Command` are all `Send +
    // 'static`, so this thread can own everything it needs.
    let handle = std::thread::spawn(move || -> Result<std::process::Child, SandboxError> {
        nono::Sandbox::apply_auto_with_abi(&caps, &abi)?;
        Ok(wrapped.spawn()?)
    });

    let child = handle.join().map_err(|_| SandboxError::ThreadPanicked)??;

    Ok(SandboxedChild {
        child,
        supervisor_report: None,
    })
}

/// Production Linux path: spawn a single-threaded trusted helper which owns
/// the only fork and the seccomp notification listener. `horizon-sessiond`
/// never calls `fork()` itself.
#[cfg(not(test))]
pub(crate) fn spawn_with_grants(
    command: Command,
    policy: &SandboxPolicy,
    filesystem_grants: &[crate::FilesystemGrant],
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    // Validate the policy and every declared root before launching the helper.
    // The helper rebuilds this same set in its own single-threaded process.
    let _ = crate::caps::build_with_grants(policy, filesystem_grants)?;
    let helper = crate::helper::resolve()?;
    let policy_json = serde_json::to_string(&crate::HelperPolicy {
        sandbox: policy.clone(),
        filesystem_grants: filesystem_grants.to_vec(),
    })?;
    let (report_reader, report_writer) = horizon_sandbox_runtime::report_channel()?;
    let report_fd = report_writer.as_raw_fd();

    let program = command.get_program().to_os_string();
    let args = command
        .get_args()
        .map(|argument| argument.to_os_string())
        .collect::<Vec<_>>();
    let mut wrapped = Command::new(helper);
    wrapped
        .arg("--supervised-linux")
        .arg(report_fd.to_string())
        .arg(policy_json)
        .arg(&program)
        .args(&args)
        .process_group(0);
    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                wrapped.env(key, value);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }
    crate::tmpdir::provision(policy, &command, &mut wrapped)?;
    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);

    let expected_parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe scalar syscalls run between fork and
    // helper exec. CLOEXEC is cleared in the child copy only, avoiding an
    // inheritable-fd window in multi-threaded sessiond.
    unsafe {
        wrapped.pre_exec(move || {
            let flags = libc::fcntl(report_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(report_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != expected_parent {
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
            }
            Ok(())
        });
    }

    let child = wrapped.spawn()?;
    let helper_pid = child.id();
    drop(report_writer);
    Ok(SandboxedChild {
        child,
        supervisor_report: Some(SupervisorReport {
            reader: report_reader,
            helper_pid,
        }),
    })
}

#[cfg(test)]
pub(crate) fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    spawn_with_grants(command, policy, &[], stdio)
}

#[cfg(test)]
mod tests;
