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
pub(crate) mod recent;
mod registry;

use std::path::PathBuf;

use crate::contract::{ToolCallId, ToolCallResult};
use crate::frame::AgentFrame;
#[cfg(test)]
use job::{run_job_body, spawn};
pub(crate) use job::{spawn_approved_host, spawn_sandboxed, BashJob, SandboxedRun};

pub(crate) use git::{
    approved_metadata_roots, git_prefilter, metadata_writable_roots, requires_metadata_write,
    GitPrefilterVerdict,
};
pub(crate) use recent::{find_reusable_output, guidance_output};

/// A bash call's outcome, delivered from the background thread that ran it
/// back to the session loop. `crates/horizon-agentd/src/session/setup.rs`
/// registers an unbounded `crossbeam_channel` per session (see
/// `register_session_runtime`) and selects on it alongside provider events,
/// folding a received completion into the session's `LiveState`/`Frames`
/// via `fold_bash_completion`.
#[derive(Clone, Debug)]
// Same justification as `ApprovalKind` in `contract.rs` -- adding
// `occurrence_id: Option<OccurrenceId>` to `ToolCallResult` pushed the
// `Finished(ToolCallResult)` variant just past the 200-byte threshold
// the lint compares against, and the variants are not constructed in a
// hot loop.
#[allow(clippy::large_enum_variant)]
pub enum ToolCompletion {
    /// An enforcing judge finished evaluating an approval candidate.
    ApprovalJudged(crate::judge::ApprovalJudgment),
    /// The call actually finished (successfully or not) -- fold
    /// `ToolCallFinished` and forward the result to the provider, exactly
    /// what every bash call did before this type grew a second variant.
    Finished(ToolCallResult),
    /// A sandboxed attempt's network egress was refused by the allowlist
    /// proxy for one or more domains (`docs/agent-approval-design.md` leg
    /// 4b). The call actually ran to completion (`result` is a genuine,
    /// already-computed outcome), but could not reach some host(s). Detected
    /// proxy-side (`SessionNetworkProxy::
    /// drain_denied_hosts`), independent of the sandboxed child's own exit
    /// code -- see `exec::run_sandboxed`'s doc comment for why that matters
    /// (backlog 59). Surfaced as a fresh, differently-named approval offer
    /// ("allow domain X for this session and retry"): approving adds
    /// `domains` to this session's allowlist and reruns the same call,
    /// still sandboxed; denying forwards `result` as-is.
    DomainDenied {
        call_id: ToolCallId,
        domains: Vec<String>,
        result: ToolCallResult,
    },
    /// A host-side web request discovered a valid next hop whose domain has
    /// not been granted to this session. No contact with that domain has
    /// occurred. `horizon-agentd` turns this into `ApprovalKind::DomainGrant`, and
    /// an approval retries the same tool call from its original URL.
    DomainGrantRequired {
        call_id: ToolCallId,
        domains: Vec<String>,
    },
    FilesystemDenied {
        call_id: ToolCallId,
        denials: Vec<horizon_sandbox::FilesystemDenial>,
        result: ToolCallResult,
    },
    /// A sandboxed attempt was refused mach-lookup to macOS security
    /// services (`docs/macos-containment-denial-reporting-design.md`) -- the
    /// macOS counterpart of `FilesystemDenied`: the call actually ran to
    /// completion, the evidence is the kernel's own denial record, and
    /// approval records the service set for this session and reruns the
    /// same call, still sandboxed; denying forwards `result` as-is.
    MachServiceDenied {
        call_id: ToolCallId,
        services: Vec<String>,
        result: ToolCallResult,
    },
}

/// Compatibility name for the bash module's existing callers. New async
/// Horizon-owned tools use [`ToolCompletion`] directly; both names refer to
/// the same per-session completion channel.
pub type BashCompletion = ToolCompletion;

/// What a [`spawn_sandboxed`] run's eventual `Finished` completion should be
/// annotated with, once it lands -- distinguishes a genuine tier-1
/// auto-approval from a human's domain-denial-retry approval (`docs/
/// agent-approval-design.md` leg 4b), so the audit trail never claims a
/// human decision was auto-approved. Never consulted for a
/// `DomainDenied`/`FilesystemDenied` completion -- both are annotated by
/// `exec::run_sandboxed` itself before this ever applies.
#[derive(Clone, Debug)]
pub(crate) enum SandboxedApprovalOrigin {
    /// `tools::execution::execute_tier1_bash`'s auto-approval path.
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

/// Kills the running child for `call_id`, if this session has one in
/// flight, and removes it from the registry. A no-op if `call_id` isn't a
/// currently-running bash call — safe to call unconditionally for every
/// provider-originated `ToolCallFinished` (see `agent::tools::processing`),
/// since a cancelled turn's synthetic `ToolCallFinished` is exactly the
/// signal that a still-running bash child needs to be killed.
pub(crate) fn kill_if_running(call_id: &ToolCallId) {
    registry::kill(call_id);
}

/// Whether a finished bash call's result should still be folded into the
/// session's frame — `false` if the *live* occurrence of `call_id` already
/// has a `ToolCallFinished` there. A cancellation racing this completion
/// (see `kill_if_running` and `agent::tools::processing`) can beat it to
/// the frame, in which case the late, genuine result is accepted and
/// discarded — the same idempotence pattern
/// `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved` uses for a
/// duplicate approve/deny. Called from
/// `horizon_agentd::session::fold_bash_completion`, on the session loop,
/// right before folding.
///
/// Occurrence-scoped rather than call_id-keyed
/// ([`AgentFrame::has_live_occurrence_finished`], see its doc comment):
/// approving a sandbox-denial retry closes the abandoned attempt with a
/// terminal result of its own, which sits after the reissued request, so
/// the call_id-keyed reading would swallow the retry's real result.
pub fn should_fold_completion(frame: &AgentFrame, call_id: &ToolCallId) -> bool {
    !frame.has_live_occurrence_finished(call_id)
}

#[cfg(test)]
mod tests;
