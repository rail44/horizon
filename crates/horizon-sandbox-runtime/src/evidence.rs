//! Cross-platform vocabulary for the strength of denial evidence.

/// How a platform can discover denied filesystem operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemDenialMode {
    /// Linux seccomp-notify can synchronously mediate `openat`/`openat2`.
    LiveOpenMediation,
    /// macOS Seatbelt violations can only be recovered from unified logs.
    PostHocBestEffort,
    /// The platform has no supported structured filesystem denial source.
    Unsupported,
}

/// Provenance carried with a structured denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialEvidence {
    /// A Linux seccomp notification whose path and liveness were revalidated.
    ValidatedSeccompOpen,
    /// A PID/time-correlated macOS Seatbelt unified-log record.
    SeatbeltUnifiedLog,
    /// Process output that merely resembles a containment failure.
    OutputHeuristic,
}

impl DenialEvidence {
    /// Whether this evidence can authoritatively name a narrow-grant request.
    ///
    /// This does not approve or apply the grant. It only permits Horizon to
    /// create a request for its normal human/judge approval flow. A seccomp
    /// notification receives the validated variant only after path resolution,
    /// canonicalization, notification-liveness checks, and policy validation.
    /// A Seatbelt unified-log record received the same trust on 2026-09-10
    /// (owner decision, `docs/macos-containment-denial-reporting-design.md`):
    /// the records are kernel-originated (user processes cannot write them),
    /// the macOS collector attributes them by pid + time window against the
    /// sampled process tree, and every grant proposed from one still passes
    /// `resolve_denial`/`revalidate_grant`. Their known incompleteness (the
    /// kernel coalesces duplicate reports) is tolerated because retries
    /// chain -- a record missed this time re-denies on the rerun. Output
    /// controlled by the sandboxed child is never authority even for naming
    /// a request.
    #[must_use]
    pub const fn is_authoritative_for_grant_request(self) -> bool {
        matches!(self, Self::ValidatedSeccompOpen | Self::SeatbeltUnifiedLog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_mediated_evidence_can_name_a_grant_request() {
        assert!(DenialEvidence::ValidatedSeccompOpen.is_authoritative_for_grant_request());
        assert!(DenialEvidence::SeatbeltUnifiedLog.is_authoritative_for_grant_request());
        assert!(!DenialEvidence::OutputHeuristic.is_authoritative_for_grant_request());
    }
}
