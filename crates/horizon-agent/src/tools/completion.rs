//! Dispatch identity and acceptance of asynchronous tool and approval outcomes.

use crate::contract::{OccurrenceId, ToolCallId, ToolCallResult};
use crate::frame::AgentFrame;

/// A tool or approval outcome, delivered from its worker
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
        domains: Vec<String>,
        result: ToolCallResult,
    },
    /// A host-side web request discovered a valid next hop whose domain has
    /// not been granted to this session. No contact with that domain has
    /// occurred. `horizon-agentd` turns this into `ApprovalKind::DomainGrant`, and
    /// an approval retries the same tool call from its original URL.
    DomainGrantRequired {
        call_id: ToolCallId,
        occurrence_id: Option<OccurrenceId>,
        domains: Vec<String>,
    },
    FilesystemDenied {
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
        services: Vec<String>,
        result: ToolCallResult,
    },
}

/// Compatibility name for the bash module's existing callers. New async
/// Horizon-owned tools use [`ToolCompletion`] directly; both names refer to
/// the same per-session completion channel.
pub type BashCompletion = ToolCompletion;

impl ToolCompletion {
    /// An explicit origin must still name the live request. Untagged legacy
    /// completions retain call-ID matching; new workers bind at dispatch time.
    pub fn matches_live_request(&self, frame: &AgentFrame) -> bool {
        let (call_id, occurrence_id) = match self {
            Self::ApprovalJudged(judgment) => (
                &judgment.candidate.request.call_id,
                &judgment.candidate.request.occurrence_id,
            ),
            Self::DomainGrantRequired {
                call_id,
                occurrence_id,
                ..
            } => (call_id, occurrence_id),
            Self::Finished(result)
            | Self::DomainDenied { result, .. }
            | Self::FilesystemDenied { result, .. }
            | Self::MachServiceDenied { result, .. } => (&result.call_id, &result.occurrence_id),
        };
        should_fold_completion(frame, call_id)
            && occurrence_id.as_ref().is_none_or(|origin| {
                frame
                    .tool_call_request(call_id)
                    .and_then(|request| request.occurrence_id.as_ref())
                    == Some(origin)
            })
    }

    /// Bind normal, denied, redirected, and panic outcomes to their dispatch.
    pub(crate) fn with_occurrence(mut self, occurrence_id: Option<OccurrenceId>) -> Self {
        if let Self::DomainGrantRequired {
            occurrence_id: origin,
            ..
        } = &mut self
        {
            *origin = occurrence_id;
        } else if let Some(result) = self.result_mut() {
            result.occurrence_id = occurrence_id;
        }
        self
    }

    pub(crate) fn result_mut(&mut self) -> Option<&mut ToolCallResult> {
        match self {
            Self::Finished(result)
            | Self::DomainDenied { result, .. }
            | Self::FilesystemDenied { result, .. }
            | Self::MachServiceDenied { result, .. } => Some(result),
            Self::ApprovalJudged(_) | Self::DomainGrantRequired { .. } => None,
        }
    }
}

/// Whether a finished bash call's result should still be folded into the
/// session's frame — `false` if the *live* occurrence of `call_id` already
/// has a `ToolCallFinished` there. A cancellation racing this completion
/// (see `cancel_call` and `agent::tools::processing`) can beat it to
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
