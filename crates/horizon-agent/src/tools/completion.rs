//! Dispatch identity and acceptance of asynchronous tool and approval outcomes.

use crate::contract::{OccurrenceId, ToolCallId, ToolCallRequest, ToolCallResult};
use crate::frame::AgentFrame;

/// A tool or approval outcome, delivered from its worker
/// back to the session loop. `crates/horizon-agent-host/src/session/run.rs`
/// registers an unbounded `crossbeam_channel` per session (see
/// `register_session_runtime`) and selects on it alongside provider events,
/// folding a received completion into the session's `LiveState`/`Frames`
/// via `fold_bash_completion`.
#[derive(Clone, Debug)]
pub enum ToolCompletion<R = ToolCallResult> {
    /// An enforcing judge finished evaluating an approval candidate.
    ApprovalJudged(Box<crate::judge::ApprovalJudgment>),
    /// The call actually finished (successfully or not) -- fold
    /// `ToolCallFinished` and forward the result to the provider, exactly
    /// what every bash call did before this type grew a second variant.
    Finished(R),
    /// A host-side web request discovered a valid next hop whose domain has
    /// not been granted to this session. No contact with that domain has
    /// occurred. `horizon-agent-host` turns this into `ApprovalKind::DomainGrant`, and
    /// an approval retries the same tool call from its original URL.
    DomainGrantRequired {
        call_id: ToolCallId,
        occurrence_id: OccurrenceId,
        domains: Vec<String>,
    },
}

/// Compatibility name for the bash module's existing callers. New async
/// Horizon-owned tools use [`ToolCompletion`] directly; both names refer to
/// the same per-session completion channel.
pub type BashCompletion = ToolCompletion;

impl ToolCompletion {
    /// A completion must name the current execution, including its occurrence.
    pub fn matches_live_request(&self, frame: &AgentFrame) -> bool {
        self.live_request(frame).is_some()
    }

    /// Accept once at the coordinator boundary, then carry the validated
    /// request into retry handling instead of looking it up by a bare ID.
    pub fn live_request<'a>(&self, frame: &'a AgentFrame) -> Option<&'a ToolCallRequest> {
        if let Self::ApprovalJudged(judgment) = self {
            let request = &judgment.candidate.request;
            if judgment.candidate.approval.identity() != request.identity()
                || !approval_is_unresolved(frame, request)
                || frame
                    .actionable_pending_approval_call_ids()
                    .contains(&request.call_id)
            {
                return None;
            }
        }
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
            Self::Finished(result) => (&result.call_id, &result.occurrence_id),
        };
        let request = frame.tool_call_request(call_id)?;
        (should_fold_completion(frame, call_id) && &request.occurrence_id == occurrence_id)
            .then_some(request)
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
/// `horizon_agent_host::session::fold_bash_completion`, on the session loop,
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

/// Decisions must still refer to the exact unresolved request. Starting an
/// asynchronous worker consumes approval just as finishing a synchronous call
/// does; a retry's terminal prior attempt also consumes that retry decision.
pub(super) fn approval_is_unresolved(frame: &AgentFrame, request: &ToolCallRequest) -> bool {
    frame.tool_call_request(&request.call_id) == Some(request)
        && !frame.approval_is_resolved(&request.identity())
        && !frame.has_tool_call_finished(&request.call_id)
        && !frame.has_tool_call_started(&request.call_id)
}
