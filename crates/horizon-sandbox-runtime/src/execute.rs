//! Minimal single-threaded Linux supervisor process boundary.
//!
//! This module deliberately contains no Horizon session or UI types. Its
//! caller is a dedicated helper executable, never the multi-threaded
//! `horizon-sessiond` host. Seccomp notification handling will be added to the
//! parent side of this fork without changing that ownership boundary.

use crate::linux::{
    handle_network_notification, handle_open_notification, install_combined_filter,
    InitialCapability, NetworkEnforcement, OpenNotificationContext, RateLimiter,
};
use crate::RecordingDenyBackend;
use crate::SupervisedOutcome;
use nono::SupervisorSocket;
use nono::{CapabilitySet, NetworkMode};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
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
    let network_enforcement = match capabilities.network_mode() {
        NetworkMode::AllowAll => None,
        NetworkMode::Blocked => Some(NetworkEnforcement::Blocked),
        NetworkMode::ProxyOnly { port, .. } => Some(NetworkEnforcement::ProxyOnly(SocketAddr::V4(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, *port),
        ))),
    };

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
        if let Err(error) =
            nono::Sandbox::apply_seccomp(&capabilities, nono::sandbox::SeccompOpts::external_tcp())
        {
            child_error_and_exit("failed to apply sandbox", &error.to_string(), 126);
        }

        // Install only after Landlock setup. CapabilitySet construction and
        // Landlock application open their rule paths; trapping those setup
        // opens before the parent owns the listener would deadlock the child.
        let notify_fd = match install_combined_filter(network_enforcement.is_some()) {
            Ok(fd) => fd,
            Err(error) => child_error_and_exit(
                "failed to install combined seccomp listener",
                &error.to_string(),
                126,
            ),
        };
        if let Err(error) = publish_listener_and_wait(&child_socket, notify_fd.as_raw_fd()) {
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
    let notify_fd = match receive_listener(&supervisor_socket, child_pid) {
        Ok(fd) => fd,
        Err(error) => {
            let _ = signal_listener_acquired(&supervisor_socket);
            unsafe {
                libc::kill(child_pid, libc::SIGKILL);
            }
            let _ = wait_for_direct_child(child_pid);
            return Err(error.into());
        }
    };
    signal_listener_acquired(&supervisor_socket)?;
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
    supervise_process_tree(
        child_pid,
        notify_fd.as_raw_fd(),
        &initial_caps,
        network_enforcement,
    )
}

fn publish_listener_and_wait(socket: &SupervisorSocket, listener_fd: RawFd) -> nono::Result<()> {
    let bytes = listener_fd.to_ne_bytes();
    write_all_raw(socket.as_raw_fd(), &bytes).map_err(|error| {
        nono::NonoError::SandboxInit(format!("failed to publish listener fd number: {error}"))
    })?;
    let mut ack = [0_u8; 1];
    read_exact_raw(socket.as_raw_fd(), &mut ack).map_err(|error| {
        nono::NonoError::SandboxInit(format!("failed to receive listener ack: {error}"))
    })?;
    if ack == [1] {
        Ok(())
    } else {
        Err(nono::NonoError::SandboxInit(
            "supervisor returned an invalid listener ack".to_string(),
        ))
    }
}

fn receive_listener(socket: &SupervisorSocket, child_pid: libc::pid_t) -> nono::Result<OwnedFd> {
    let child_fd = socket.recv_raw_fd_number()?;
    let pidfd_raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child_pid, 0_u32) };
    if pidfd_raw < 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "pidfd_open failed for sandbox child {child_pid}: {}",
            std::io::Error::last_os_error()
        )));
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd_raw as RawFd) };
    let listener =
        unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), child_fd, 0_u32) };
    if listener < 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "pidfd_getfd failed for sandbox listener fd {child_fd}: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(listener as RawFd) })
}

fn signal_listener_acquired(socket: &SupervisorSocket) -> nono::Result<()> {
    write_all_raw(socket.as_raw_fd(), &[1]).map_err(|error| {
        nono::NonoError::SandboxInit(format!("failed to acknowledge listener fd: {error}"))
    })
}

fn write_all_raw(fd: RawFd, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

fn read_exact_raw(fd: RawFd, mut bytes: &mut [u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        if read < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let (_, rest) = std::mem::take(&mut bytes).split_at_mut(read as usize);
        bytes = rest;
    }
    Ok(())
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
    network_enforcement: Option<NetworkEnforcement>,
) -> Result<SupervisedOutcome, ExecuteError> {
    let backend = RecordingDenyBackend::default();
    let mut limiter = RateLimiter::new(10, 5);
    let mut denials = Vec::new();
    let mut ipc_denials = Vec::new();
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
            let notification = nono::sandbox::recv_notif(notify_fd)?;
            match notification.data.nr {
                nono::sandbox::SYS_OPENAT | nono::sandbox::SYS_OPENAT2 => {
                    handle_open_notification(
                        notify_fd,
                        notification,
                        OpenNotificationContext {
                            child_pid: direct_child as u32,
                            session_id: "horizon-sandbox-helper",
                            initial_caps,
                            backend: &backend,
                            rate_limiter: &mut limiter,
                            denials: &mut denials,
                        },
                    )?;
                }
                _ => match network_enforcement {
                    Some(enforcement) => handle_network_notification(
                        notify_fd,
                        notification,
                        enforcement,
                        &mut ipc_denials,
                    )?,
                    None => nono::sandbox::deny_notif(notify_fd, notification.id)?,
                },
            }
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
                    ipc_denials,
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
