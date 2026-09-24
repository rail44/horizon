//! Apply tool lifecycle events before publishing them or returning a result.
//! Called by the session coordinator; workers only send ToolCompletion values.
//! LiveState enqueues persistence as usual; an update is not a disk-flush receipt.
use crate::contract::{Event, SessionState, ToolCallIdentity, ToolCallRequest, ToolCallResult};
use crate::live::LiveState;

/// An update applied to LiveState, consumed by the daemon to publish events before handing
/// the terminal result to the provider. Starting a worker has no result yet.
#[derive(Debug)]
pub enum ToolUpdate {
    Started {
        events: Vec<Event>,
    },
    Finished {
        events: Vec<Event>,
        result: ToolCallResult,
    },
}

impl ToolUpdate {
    pub(crate) fn start(
        live: &LiveState,
        request: &ToolCallRequest,
        prior: Option<&ToolCallResult>,
    ) -> Self {
        let mut events = Vec::new();
        if let Some(prior) = prior {
            events.push(Event::ToolCallFinished(
                prior.superseded_by_retry(&request.occurrence_id),
            ));
        }
        events.extend(start_events(request.identity()));
        apply(live, &events);
        Self::Started { events }
    }

    /// Synchronous approval execution has no intervening coordinator turn.
    pub(crate) fn executed(
        live: &LiveState,
        result: ToolCallResult,
        identity: ToolCallIdentity,
    ) -> Self {
        Self::finish_with_events(live, result, start_events(identity))
    }

    /// The caller has accepted this live attempt (or an unstarted refusal).
    pub fn finish(live: &LiveState, result: ToolCallResult) -> Self {
        Self::finish_with_events(live, result, Vec::new())
    }

    /// Declining a retry settles the attempt that actually ran, then closes
    /// the unexecuted offer. Only the genuine result answers the provider.
    pub(crate) fn decline_retry(live: &LiveState, result: ToolCallResult) -> Self {
        let request = live.frame().tool_call_request(&result.call_id).cloned();
        let mut events = vec![Event::ToolCallFinished(result.clone())];
        if let Some(request) = request {
            if request.occurrence_id != result.occurrence_id {
                events.push(Event::ToolCallFinished(ToolCallResult::cancelled(
                    request.identity(),
                )));
            }
        }
        Self::finish_and_apply(live, events, result)
    }

    fn finish_with_events(
        live: &LiveState,
        result: ToolCallResult,
        mut events: Vec<Event>,
    ) -> Self {
        events.push(Event::ToolCallFinished(result.clone()));
        Self::finish_and_apply(live, events, result)
    }

    fn finish_and_apply(live: &LiveState, mut events: Vec<Event>, result: ToolCallResult) -> Self {
        // The provider still owes the next round. A sibling approval remains
        // actionable until its own result lands; neither case ends the turn.
        let waiting = live
            .frame()
            .actionable_pending_approval_call_ids()
            .into_iter()
            .any(|id| id != result.call_id);
        events.push(Event::StateChanged(if waiting {
            SessionState::WaitingForApproval
        } else {
            SessionState::Running
        }));
        apply(live, &events);
        Self::Finished { events, result }
    }
}

fn start_events(identity: ToolCallIdentity) -> Vec<Event> {
    vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(identity),
    ]
}

fn apply(live: &LiveState, events: &[Event]) {
    live.extend_provider_events(events.iter().cloned().map(Into::into));
}
