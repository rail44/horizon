//! Apply tool lifecycle events before publishing them or returning a result.
//! Called by the session coordinator; workers only send ToolCompletion values.
//! Persistent events are acknowledged before an update can be published.
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
    ) -> Result<Self, String> {
        let mut events = Vec::new();
        if let Some(prior) = prior {
            events.push(Event::ToolCallFinished(
                prior.superseded_by_retry(&request.occurrence_id),
            ));
        }
        events.extend(start_events(request.identity()));
        apply(live, &events)?;
        Ok(Self::Started { events })
    }

    /// Save the start before a synchronous side effect and save its result
    /// before handing the complete publication batch back to the coordinator.
    pub(crate) fn execute(
        live: &LiveState,
        request: &ToolCallRequest,
        operation: impl FnOnce() -> serde_json::Value,
    ) -> Result<Self, String> {
        let mut events = start_events(request.identity());
        apply(live, &events)?;
        let result = request.identity().result(operation());
        let finished = finish_events(live, &result, vec![Event::ToolCallFinished(result.clone())]);
        apply(live, &finished)?;
        events.extend(finished);
        Ok(Self::Finished { events, result })
    }

    /// The caller has accepted this live attempt (or an unstarted refusal).
    pub fn finish(live: &LiveState, result: ToolCallResult) -> Result<Self, String> {
        Self::finish_with_events(live, result, Vec::new())
    }

    /// Declining a retry settles the attempt that actually ran, then closes
    /// the unexecuted offer. Only the genuine result answers the provider.
    pub(crate) fn decline_retry(live: &LiveState, result: ToolCallResult) -> Result<Self, String> {
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
    ) -> Result<Self, String> {
        events.push(Event::ToolCallFinished(result.clone()));
        Self::finish_and_apply(live, events, result)
    }

    fn finish_and_apply(
        live: &LiveState,
        mut events: Vec<Event>,
        result: ToolCallResult,
    ) -> Result<Self, String> {
        events = finish_events(live, &result, events);
        apply(live, &events)?;
        Ok(Self::Finished { events, result })
    }
}

fn finish_events(live: &LiveState, result: &ToolCallResult, mut events: Vec<Event>) -> Vec<Event> {
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
    events
}

fn start_events(identity: ToolCallIdentity) -> Vec<Event> {
    vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(identity),
    ]
}

fn apply(live: &LiveState, events: &[Event]) -> Result<(), String> {
    live.extend_provider_events(events.iter().cloned().map(Into::into))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{OccurrenceId, SessionId, ToolCallId};
    use crate::persistence::event_log::{WriterHandle, WriterInit};

    #[test]
    fn a_failed_start_cannot_run_a_synchronous_approved_effect_or_publish_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, ready) = WriterHandle::open(dir.path());
        assert!(matches!(ready.recv().unwrap(), WriterInit::Failed(_)));
        let request = ToolCallRequest {
            call_id: ToolCallId("blocked".into()),
            occurrence_id: OccurrenceId::new(),
            tool_id: "fs.write".into(),
            input: serde_json::json!({}).into(),
        };
        let history = vec![Event::ToolCallRequested(request.clone())];
        let live = LiveState::with_event_log_and_history(
            SessionId::new(),
            None,
            None,
            writer,
            history.clone(),
        );
        assert!(
            ToolUpdate::execute(&live, &request, || panic!("side effect must not run")).is_err()
        );
        assert!(ToolUpdate::finish(
            &live,
            request
                .identity()
                .result(serde_json::json!({"finished":true}))
        )
        .is_err());
        assert_eq!(live.events(), history);
    }
}
