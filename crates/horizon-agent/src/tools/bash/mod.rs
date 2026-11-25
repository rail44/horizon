//! The `bash` tool starts a fresh process per call and tracks cwd across
//! calls. [`BashJob`] captures thread-safe inputs; a call runs contained by
//! the sandbox runtime or, once approved, with the host's authority. Both
//! enqueue work through one per-session FIFO and deliver completions to the
//! host session loop without blocking it.
//!
//! Job panic handling sends a failure completion, while the registry's
//! advance-on-drop guard independently keeps later jobs from getting stuck.

mod cargo;
mod exec;
mod git;
pub(crate) mod job;
mod output;
pub(crate) mod process;
mod registry;
mod shell;

pub(super) use super::completion::BashCompletion;
#[cfg(test)]
use super::completion::should_fold_completion;
pub(crate) use job::{BashJob, ContainedOrigin, spawn_approved_host, spawn_contained};
#[cfg(test)]
use job::{run_job_body, spawn};
#[cfg(test)]
pub(crate) use registry::cancel_call;

pub(crate) use git::metadata_writable_roots;

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
