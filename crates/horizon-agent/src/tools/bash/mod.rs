//! The `bash` tool starts a fresh process per call and tracks cwd across
//! calls. [`BashJob`] captures thread-safe inputs; [`SandboxedRun`] captures
//! the grants and approval origin for sandboxed execution. Both execution
//! modes enqueue work through one per-session FIFO and deliver completions
//! to the daemon session loop without blocking it.
//!
//! Job panic handling sends a failure completion, while the registry's
//! advance-on-drop guard independently keeps later jobs from getting stuck.
//! The daemon folds each completion into live state and resolves retries
//! (`crates/horizon-agentd/src/session/completion.rs`).

mod cargo;
mod exec;
mod git;
mod job;
mod output;
mod process;
pub(crate) mod recent;
mod registry;
mod shell;

use std::path::PathBuf;

#[cfg(test)]
use super::completion::should_fold_completion;
pub(super) use super::completion::BashCompletion;
#[cfg(test)]
use job::{run_job_body, spawn};
pub(crate) use job::{spawn_approved_host, spawn_sandboxed, BashJob, SandboxedRun};
pub(crate) use registry::{cancel_call, cancel_session};

pub(crate) use git::{
    approved_metadata_roots, git_prefilter, metadata_writable_roots, requires_metadata_write,
    GitPrefilterVerdict,
};
pub(crate) use recent::{find_reusable_output, guidance_output};

/// What a [`spawn_sandboxed`] run's eventual `Finished` completion should be
/// annotated with, once it lands -- distinguishes a genuine tier-1
/// auto-approval from a human's domain-denial-retry approval (`docs/
/// agent-approval-design.md` leg 4b), so the audit trail never claims a
/// human decision was auto-approved. Never consulted for a
/// `DomainDenied`/`FilesystemDenied` completion -- both are annotated by
/// `exec::run_sandboxed` itself before this ever applies.
#[derive(Clone, Debug)]
pub(crate) enum SandboxedApprovalOrigin {
    /// `tools::execution::execute_automatic`'s auto-approval path.
    Tier1Auto,
    /// `tools::approval`'s domain-denial-retry approve path -- a human
    /// decision, carrying the domain(s) they just approved for this
    /// session.
    ManualDomainRetry { domains: Vec<String> },
    /// A human approved the validated Git metadata roots for this call.
    ManualGitOperation,
    /// A filesystem-denial retry (`docs/containment-denial-narrow-grants-
    /// design.md`'s 2026-07-26 decision): `grants` is the scoped authority
    /// this rerun actually received, and `trigger_paths` the mediated
    /// attempts that prompted the request. Stated separately because they
    /// are different claims -- the grants bound the execution, the trigger
    /// paths only explain why it was offered.
    FilesystemGrant {
        source: ApprovalSource,
        grants: Vec<horizon_sandbox::FilesystemGrant>,
        trigger_paths: Vec<PathBuf>,
    },
    /// A judge/human-approved mach service grant: the approval recorded the
    /// service set on this session's state, and this call is the sandboxed
    /// retry that runs with the enforcement grants assembled from it
    /// (`docs/macos-containment-denial-reporting-design.md`).
    MachServiceGrant { services: Vec<String> },
}

/// Who decided an approval -- the enforcing judge or a human. Both produce
/// the same trusted effect; recording which is an audit requirement, not a
/// policy input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalSource {
    Human,
    Judge,
}

impl ApprovalSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Judge => "judge",
        }
    }
}

/// An [`ApprovalKind::Standard`](crate::contract::ApprovalKind::Standard)
/// `bash` approval: the one remaining path that runs a call with the host
/// process's ordinary authority, which is what an ordinary bash approval
/// has always meant. Filesystem-denial retries no longer come through here
/// -- they rerun sandboxed with an explicit grant instead (see
/// [`SandboxedApprovalOrigin::FilesystemGrant`]).
#[derive(Clone, Debug)]
pub(crate) struct HostExecutionApproval {
    source: ApprovalSource,
}

impl HostExecutionApproval {
    pub(crate) fn new(source: ApprovalSource) -> Self {
        Self { source }
    }
}

#[cfg(test)]
mod tests;
