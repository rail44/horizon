//! Horizon's local extraction boundary for nono's supervised runtime.
//!
//! It contains the OS-neutral approval/audit contract and the reduced Linux
//! recording-deny supervisor without nono-cli's terminal, session, rollback,
//! trust, live-grant, and profile layers. Process supervision runs only through
//! a dedicated helper process; it never forks inside the multi-threaded
//! `horizon-sessiond` host.
//!
//! The derived source revision and local deviations are recorded in
//! `UPSTREAM.md` next to this crate.

mod approval;
mod evidence;
#[cfg(target_os = "linux")]
mod execute;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod report;

pub use approval::RecordingDenyBackend;
pub use evidence::{DenialEvidence, FilesystemDenialMode};
#[cfg(target_os = "linux")]
pub use execute::{execute, ExecuteError};
#[cfg(target_os = "linux")]
pub use linux::SeccompPolicy;
#[cfg(target_os = "linux")]
pub use report::{report_channel, ReportError, ReportReader, ReportWriter};

/// Structured result returned by the reduced supervisor helper.
///
/// Defining the result at the extraction boundary prevents the eventual
/// helper transport from leaking Horizon agent/UI types into this crate.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SupervisedOutcome {
    /// Exit status of the sandboxed command using shell-compatible semantics.
    pub exit_code: i32,
    /// Filesystem denials recorded by nono's mediation layer.
    pub denials: Vec<nono::DenialRecord>,
    /// Network or Unix-socket denials recorded by nono's mediation layer.
    pub ipc_denials: Vec<nono::IpcDenialRecord>,
    /// Requests and fail-closed decisions made by the configured backend.
    pub approvals: Vec<nono::supervisor::AuditEntry>,
}

impl SupervisedOutcome {
    /// Creates the behavior-neutral outcome used before live mediation is enabled.
    #[must_use]
    pub fn completed(exit_code: i32) -> Self {
        Self {
            exit_code,
            denials: Vec::new(),
            ipc_denials: Vec::new(),
            approvals: Vec::new(),
        }
    }
}
