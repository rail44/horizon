//! Horizon's local extraction boundary for nono's supervised runtime.
//!
//! This first slice intentionally contains only the OS-neutral approval/audit
//! contract and the small Linux policy primitives that can be copied without
//! importing nono-cli's terminal, session, rollback, trust, and profile layers.
//! Process supervision will be connected through a dedicated helper process;
//! it must not fork inside the multi-threaded `horizon-sessiond` host.
//!
//! The derived source revision and local deviations are recorded in
//! `UPSTREAM.md` next to this crate.

mod approval;
mod evidence;
#[cfg(target_os = "linux")]
mod linux;

pub use approval::RecordingDenyBackend;
pub use evidence::{DenialEvidence, FilesystemDenialMode};
#[cfg(target_os = "linux")]
pub use linux::SeccompPolicy;

/// Structured result returned by the future reduced supervisor helper.
///
/// Defining the result at the extraction boundary prevents the eventual
/// helper transport from leaking Horizon agent/UI types into this crate.
#[derive(Debug)]
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
