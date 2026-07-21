//! Minimal single-threaded Linux supervisor process boundary.
//!
//! This module deliberately contains no Horizon session or UI types. Its
//! caller is a dedicated helper executable, never the multi-threaded
//! `horizon-sessiond` host. Seccomp notification handling will be added to the
//! parent side of this fork without changing that ownership boundary.

use crate::linux::{handle_open_notification, InitialCapability, RateLimiter};
use crate::RecordingDenyBackend;
use crate::SupervisedOutcome;
use nono::CapabilitySet;
use nono::SupervisorSocket;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Failures before or while supervising the sandboxed child.
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("the supervised runtime must start single-threaded, found {0} threads")]
    MultipleThreads(usize),
    #[error("failed to inspect helper thread state: {0}")]
    ThreadInspection(#[source] std::io::Error),
    #[error("failed to configure the helper as a child subreaper: {0}")]
    Subreaper(#[source] std::io::Error),
    #[error("fork failed: {0}")]
    Fork(#[source] std::io::Error),
    #[error("waitpid failed: {0}")]
    Wait(#[source] std::io::Error),
    #[error("nono supervisor setup or notification handling failed: {0}")]
    Nono(#[from] nono::NonoError),
    #[error("the sandboxed process tree ended without a direct-child status")]
    MissingChildStatus,
    #[error("the helper's parent changed while arming parent-death protection")]
    ParentChanged,
}

/// Runs one command beneath a dedicated unsandboxed supervisor process.
///
/// `close_in_child` names trusted helper-only descriptors, most importantly
/// the structured report memfd. They are closed before sandbox setup and exec,
/// so untrusted target code cannot forge the supervisor's report.
pub fn execute(
    mut command: Command,
    mut capabilities: CapabilitySet,
    close_in_child: &[RawFd],
) -> Result<SupervisedOutcome, ExecuteError> {
    require_single_threaded()?;
    become_child_subreaper()?;
    let helper_parent = unsafe { libc::getppid() };
    arm_parent_death_signal(helper_parent)?;
    let helper_pid = unsafe { libc::getpid() };
    let (supervisor_socket, child_socket) = SupervisorSocket::pair()?;

    // SAFETY: this function rejects a multi-threaded helper before forking.
    // The child owns its copy of `command` and `capabilities`, applies only
    // one-way restrictions, then immediately execs or `_exit`s.
    let child_pid = unsafe { libc::fork() };
    if child_pid < 0 {
        return Err(ExecuteError::Fork(std::io::Error::last_os_error()));
    }

    if child_pid == 0 {
        drop(supervisor_socket);
        for fd in close_in_child {
            // SAFETY: closing an inherited descriptor is async-signal-safe.
            unsafe {
                libc::close(*fd);
            }
        }

        if let Err(error) = arm_parent_death_signal_raw(helper_pid) {
            child_error_and_exit(
                "failed to arm parent-death protection",
                &error.to_string(),
                126,
            );
        }

        capabilities.remap_procfs_self_references(std::process::id(), None);
        capabilities.widen_procfs_self_to_proc();
        if let Err(error) = nono::Sandbox::apply_auto(&capabilities) {
            child_error_and_exit("failed to apply sandbox", &error.to_string(), 126);
        }

        // Install only after Landlock setup. CapabilitySet construction and
        // Landlock application open their rule paths; trapping those setup
        // opens before the parent owns the listener would deadlock the child.
        let notify_fd = match nono::sandbox::install_seccomp_notify() {
            Ok(fd) => fd,
            Err(error) => child_error_and_exit(
                "failed to install openat seccomp listener",
                &error.to_string(),
                126,
            ),
        };
        if let Err(error) = child_socket.send_fd(notify_fd.as_raw_fd()) {
            child_error_and_exit(
                "failed to transfer seccomp listener",
                &error.to_string(),
                126,
            );
        }
        drop(notify_fd);
        drop(child_socket);

        let error = command.exec();
        child_error_and_exit("failed to exec command", &error.to_string(), 127);
    }

    drop(child_socket);
    harden_supervisor_parent()?;
    let notify_fd = match supervisor_socket.recv_fd() {
        Ok(fd) => fd,
        Err(error) => {
            let _ = wait_for_direct_child(child_pid);
            return Err(error.into());
        }
    };
    drop(supervisor_socket);

    let mut supervisor_caps = capabilities;
    supervisor_caps.remap_procfs_self_references(child_pid as u32, None);
    let initial_caps = supervisor_caps
        .fs_capabilities()
        .iter()
        .map(|capability| InitialCapability {
            path: capability.resolved.clone(),
            access: capability.access,
            is_file: capability.is_file,
        })
        .collect::<Vec<_>>();
    supervise_process_tree(child_pid, notify_fd.as_raw_fd(), &initial_caps)
}

fn require_single_threaded() -> Result<(), ExecuteError> {
    let count = std::fs::read_dir("/proc/self/task")
        .map_err(ExecuteError::ThreadInspection)?
        .try_fold(0usize, |count, entry| {
            entry.map(|_| count.saturating_add(1))
        })
        .map_err(ExecuteError::ThreadInspection)?;
    if count == 1 {
        Ok(())
    } else {
        Err(ExecuteError::MultipleThreads(count))
    }
}

fn become_child_subreaper() -> Result<(), ExecuteError> {
    // The later seccomp supervisor must retain ancestry over daemonizing
    // descendants so `/proc/<pid>/mem` inspection remains ptrace-authorized.
    // Establish the invariant now, before live notification handling lands.
    // SAFETY: prctl receives scalar arguments and touches no Rust memory.
    let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(ExecuteError::Subreaper(std::io::Error::last_os_error()))
    }
}

fn arm_parent_death_signal(expected_parent: libc::pid_t) -> Result<(), ExecuteError> {
    arm_parent_death_signal_raw(expected_parent).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ExecuteError::ParentChanged
        } else {
            ExecuteError::Subreaper(error)
        }
    })
}

fn arm_parent_death_signal_raw(expected_parent: libc::pid_t) -> std::io::Result<()> {
    if unsafe { libc::getppid() } != expected_parent {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    // SAFETY: prctl receives scalar arguments and touches no Rust memory.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != expected_parent {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    Ok(())
}

fn harden_supervisor_parent() -> Result<(), ExecuteError> {
    // The child must remain inspectable by its direct ancestor for seccomp
    // path reads. The unsandboxed helper itself has no such need and becomes
    // non-dumpable immediately after fork.
    // SAFETY: prctl receives scalar arguments and touches no Rust memory.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0 {
        Ok(())
    } else {
        Err(ExecuteError::Subreaper(std::io::Error::last_os_error()))
    }
}

fn supervise_process_tree(
    direct_child: libc::pid_t,
    notify_fd: RawFd,
    initial_caps: &[InitialCapability],
) -> Result<SupervisedOutcome, ExecuteError> {
    let backend = RecordingDenyBackend::default();
    let mut limiter = RateLimiter::new(10, 5);
    let mut denials = Vec::new();
    let mut direct_status = None;

    loop {
        let mut pollfd = libc::pollfd {
            fd: notify_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to one writable descriptor record.
        let polled = unsafe { libc::poll(&mut pollfd, 1, 50) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(ExecuteError::Wait(error));
            }
        } else if polled > 0 && pollfd.revents & libc::POLLIN != 0 {
            handle_open_notification(
                notify_fd,
                direct_child as u32,
                "horizon-sandbox-helper",
                initial_caps,
                &backend,
                &mut limiter,
                &mut denials,
            )?;
        }

        loop {
            let mut status = 0;
            // Reap the direct child and any daemonizing descendants adopted
            // because this helper is a subreaper.
            // SAFETY: status is writable and -1/WNOHANG are valid waitpid args.
            let waited = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if waited > 0 {
                if waited == direct_child {
                    direct_status = Some(shell_exit_code(status));
                }
                continue;
            }
            if waited == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                let exit_code = direct_status.ok_or(ExecuteError::MissingChildStatus)?;
                return Ok(SupervisedOutcome {
                    exit_code,
                    denials,
                    ipc_denials: Vec::new(),
                    approvals: backend.drain(),
                });
            }
            return Err(ExecuteError::Wait(error));
        }
    }
}

fn wait_for_direct_child(child_pid: libc::pid_t) -> Result<i32, ExecuteError> {
    loop {
        let mut status = 0;
        // SAFETY: child_pid came from fork and status is writable.
        let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
        if waited == child_pid {
            return Ok(shell_exit_code(status));
        }
        if waited < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(ExecuteError::Wait(error));
        }
    }
}

fn shell_exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn child_error_and_exit(context: &str, detail: &str, code: i32) -> ! {
    let message = format!("horizon-sandbox-helper: {context}: {detail}\n");
    // SAFETY: the byte slice is valid for the duration of the write. Ignore a
    // short/error write because this is already the terminal setup path.
    unsafe {
        libc::write(
            libc::STDERR_FILENO,
            message.as_ptr().cast::<libc::c_void>(),
            message.len(),
        );
        libc::_exit(code);
    }
}
