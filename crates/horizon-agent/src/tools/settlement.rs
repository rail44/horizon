//! Settle stopped provider batches against the host's acknowledged results.
use super::{ToolUpdate, processing::Processing};
use crate::{
    contract::{Command, ProviderEvent, SessionId, ToolCallIdentity, ToolCallResult, ToolOutcome},
    frame::{AgentFrameItem, tool_call_occurrences},
    live::LiveState,
};

pub(super) fn settle(
    live: &LiveState,
    session: SessionId,
    id: String,
    calls: Vec<ToolCallIdentity>,
) -> Result<Processing, String> {
    let mut events = Vec::new();
    let mut results = Vec::new();
    for mut identity in calls {
        loop {
            let frame = live.frame();
            let occurrences = tool_call_occurrences(&frame.items);
            let call = occurrences
                .iter()
                .find(|call| call.request.identity() == identity)
                .ok_or_else(|| {
                    format!(
                        "Cannot settle unknown tool occurrence: {}",
                        identity.occurrence_id.0
                    )
                })?;
            if let Some(result) = call.result {
                if let ToolOutcome::Superseded {
                    retry_occurrence_id,
                } = &result.outcome
                {
                    identity.occurrence_id = retry_occurrence_id.clone();
                    continue;
                }
                results.push(result.clone());
                break;
            }
            // A completed denial can be held inside a pending retry offer.
            // Closing that offer must preserve the result of the attempt that ran.
            let prior = frame.items.iter().rev().find_map(|item| match item {
                AgentFrameItem::ApprovalRequested(approval) => {
                    let result = approval.kind.prior_result()?;
                    (result.call_id == identity.call_id
                        && result.occurrence_id == identity.occurrence_id)
                        .then_some(result.clone())
                }
                _ => None,
            });
            super::cancel_tool_execution(session, &identity);
            let update = if let Some(prior) = prior {
                ToolUpdate::decline_retry(live, prior)?
            } else {
                ToolUpdate::finish(live, ToolCallResult::cancelled(identity))?
            };
            let ToolUpdate::Finished {
                events: finished,
                result,
            } = update
            else {
                unreachable!()
            };
            events.extend(finished.into_iter().map(ProviderEvent::from));
            results.push(result);
            break;
        }
    }
    Ok(Processing {
        horizon_events: events,
        provider_commands: vec![Command::ToolCallsSettled { id, results }],
        approval: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Event, OccurrenceId, ToolCallId, ToolCallRequest};
    fn request(call: &str, occurrence: &str) -> ToolCallRequest {
        ToolCallRequest {
            call_id: ToolCallId(call.into()),
            occurrence_id: OccurrenceId(occurrence.into()),
            tool_id: "bash".into(),
            input: serde_json::json!({"command":"test"}).into(),
        }
    }
    fn results(processing: Processing) -> Vec<ToolCallResult> {
        let [Command::ToolCallsSettled { results, .. }] = &processing.provider_commands[..] else {
            panic!("settlement receipt")
        };
        results.clone()
    }
    #[test]
    fn settlement_keeps_completed_results_and_cancels_only_unfinished_exact_attempts() {
        let live = LiveState::with_disabled_persistence();
        let done = request("same", "old");
        let fresh = request("same", "new");
        let completed = done
            .identity()
            .result(serde_json::json!({"output":"actual effect"}));
        live.extend_events([
            Event::ToolCallRequested(done.clone()),
            Event::ToolCallFinished(completed.clone()),
            Event::ToolCallRequested(fresh.clone()),
        ]);
        let session = SessionId::new();
        assert_eq!(
            results(settle(&live, session, "one".into(), vec![done.identity()]).unwrap()),
            vec![completed.clone()]
        );
        assert_eq!(live.frame().unfinished_tool_calls(), vec![&fresh]);
        let cancelled =
            results(settle(&live, session, "two".into(), vec![fresh.identity()]).unwrap());
        assert_eq!(cancelled, [ToolCallResult::cancelled(fresh.identity())]);
        let count = live.events().len();
        assert_eq!(
            results(settle(&live, session, "again".into(), vec![fresh.identity()]).unwrap()),
            cancelled
        );
        assert_eq!(
            live.events().len(),
            count,
            "settlement never writes another terminal result"
        );
    }
}
