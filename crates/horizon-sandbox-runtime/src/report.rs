//! Authenticated structured-report transport for the Linux helper.
//!
//! The report travels over a private `SOCK_SEQPACKET` socketpair, not target
//! stdout/stderr or a target-visible filesystem path. The receiving endpoint
//! asks Linux for `SCM_CREDENTIALS` and verifies that the packet came from the
//! exact helper PID that Horizon spawned. The real target child closes the
//! sending endpoint before sandbox setup and exec.

use crate::SupervisedOutcome;
use serde::{Deserialize, Serialize};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const WIRE_VERSION: u16 = 1;
// Kept comfortably below Linux's default AF_UNIX send buffer so the helper
// cannot block behind a caller that waits before receiving.
const MAX_REPORT_BYTES: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct WireReport {
    version: u16,
    outcome: SupervisedOutcome,
}

/// Parent-side endpoint retained by Horizon until the helper reports.
#[derive(Debug)]
pub struct ReportReader(OwnedFd);

/// Helper-side endpoint inherited across exactly the helper exec.
#[derive(Debug)]
pub struct ReportWriter(OwnedFd);

/// Creates a private packet socket and enables kernel sender credentials.
pub fn report_channel() -> std::io::Result<(ReportReader, ReportWriter)> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` points to two writable integers. CLOEXEC starts enabled on
    // both ends; only the trusted helper end is explicitly cleared below.
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two fresh owned descriptors.
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: socketpair returned two fresh owned descriptors.
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    let enabled: libc::c_int = 1;
    // SAFETY: the reader is valid and `enabled` has the expected scalar type.
    let passcred = unsafe {
        libc::setsockopt(
            reader.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&enabled as *const libc::c_int).cast::<libc::c_void>(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if passcred != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok((ReportReader(reader), ReportWriter(writer)))
}

impl ReportReader {
    /// Receives one bounded report and authenticates its kernel-supplied PID.
    pub fn read(self, expected_helper_pid: u32) -> Result<SupervisedOutcome, ReportError> {
        let mut bytes = vec![0_u8; MAX_REPORT_BYTES];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast::<libc::c_void>(),
            iov_len: bytes.len(),
        };
        let control_len = unsafe { libc::CMSG_SPACE(size_of::<libc::ucred>() as u32) as usize };
        let mut control = vec![0_u8; control_len];
        // SAFETY: zero is a valid initial state for msghdr; all pointers below
        // are populated with live buffers before recvmsg.
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
        message.msg_controllen = control.len();

        // SAFETY: message points to valid iovec and control buffers.
        let received = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut message, 0) };
        if received < 0 {
            return Err(ReportError::Io(std::io::Error::last_os_error()));
        }
        if received == 0 {
            return Err(ReportError::Missing);
        }
        if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
            return Err(ReportError::TooLarge(received as usize));
        }

        let credentials = credentials_from(&message).ok_or(ReportError::MissingCredentials)?;
        let expected_pid = i32::try_from(expected_helper_pid)
            .map_err(|_| ReportError::UnexpectedSender(credentials.pid))?;
        // SAFETY: geteuid/getegid have no arguments and no failure mode.
        let (expected_uid, expected_gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        if credentials.pid != expected_pid
            || credentials.uid != expected_uid
            || credentials.gid != expected_gid
        {
            return Err(ReportError::UnexpectedSender(credentials.pid));
        }

        bytes.truncate(received as usize);
        let report: WireReport = serde_json::from_slice(&bytes)?;
        if report.version != WIRE_VERSION {
            return Err(ReportError::UnsupportedVersion(report.version));
        }
        Ok(report.outcome)
    }
}

impl ReportWriter {
    /// Reconstructs the endpoint inherited by the trusted helper process.
    ///
    /// # Safety
    ///
    /// `fd` must be an owned, valid descriptor not represented by another
    /// owner in this process.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        // SAFETY: upheld by the caller.
        Self(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Descriptor that must be closed in the real target child.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    /// Sends exactly one bounded, versioned packet.
    pub fn write(self, outcome: SupervisedOutcome) -> Result<(), ReportError> {
        let mut report = WireReport {
            version: WIRE_VERSION,
            outcome,
        };
        let bytes = loop {
            let bytes = serde_json::to_vec(&report)?;
            if bytes.len() <= MAX_REPORT_BYTES {
                break bytes;
            }
            if report.outcome.denials.pop().is_some() || report.outcome.ipc_denials.pop().is_some()
            {
                continue;
            }
            if report.outcome.approvals.len() > 1 {
                report.outcome.approvals.pop();
                continue;
            }
            return Err(ReportError::TooLarge(bytes.len()));
        };
        // SAFETY: bytes is a live buffer and the descriptor is a connected
        // SOCK_SEQPACKET endpoint. MSG_NOSIGNAL avoids process-wide SIGPIPE.
        let sent = unsafe {
            libc::send(
                self.0.as_raw_fd(),
                bytes.as_ptr().cast::<libc::c_void>(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            return Err(ReportError::Io(std::io::Error::last_os_error()));
        }
        if sent as usize != bytes.len() {
            return Err(ReportError::ShortWrite {
                expected: bytes.len(),
                actual: sent as usize,
            });
        }
        Ok(())
    }
}

fn credentials_from(message: &libc::msghdr) -> Option<libc::ucred> {
    // SAFETY: the kernel initialized the control chain within the supplied
    // buffer. CMSG helpers only walk that bounded chain.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET
                && (*header).cmsg_type == libc::SCM_CREDENTIALS
                && (*header).cmsg_len as usize
                    >= libc::CMSG_LEN(size_of::<libc::ucred>() as u32) as usize
            {
                return Some(std::ptr::read_unaligned(
                    libc::CMSG_DATA(header).cast::<libc::ucred>(),
                ));
            }
            header = libc::CMSG_NXTHDR(message, header);
        }
    }
    None
}

/// Structured report transport failures.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("the sandbox helper exited without writing a structured report")]
    Missing,
    #[error("the sandbox helper report lacked kernel sender credentials")]
    MissingCredentials,
    #[error("the sandbox helper report came from unexpected pid {0}")]
    UnexpectedSender(i32),
    #[error("sandbox helper report version {0} is unsupported")]
    UnsupportedVersion(u16),
    #[error("the sandbox helper report exceeded its {MAX_REPORT_BYTES}-byte limit ({0} bytes)")]
    TooLarge(usize),
    #[error("sandbox helper report write was short (expected {expected}, wrote {actual})")]
    ShortWrite { expected: usize, actual: usize },
    #[error("sandbox helper report I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("sandbox helper report JSON was invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trip_verifies_kernel_sender_credentials() {
        let (reader, writer) = report_channel().expect("create report channel");
        writer
            .write(SupervisedOutcome::completed(23))
            .expect("write report");
        let outcome = reader
            .read(std::process::id())
            .expect("read authenticated report");
        assert_eq!(outcome.exit_code, 23);
        assert!(outcome.approvals.is_empty());
    }

    #[test]
    fn report_rejects_a_different_expected_pid() {
        let (reader, writer) = report_channel().expect("create report channel");
        writer
            .write(SupervisedOutcome::completed(0))
            .expect("write report");
        let error = reader
            .read(std::process::id().saturating_add(1))
            .expect_err("sender pid must be checked");
        assert!(matches!(error, ReportError::UnexpectedSender(_)));
    }
}
