//! Correlate every lifecycle event with its exact execution request.

use super::{AgentFrame, AgentFrameItem};
use crate::contract::{OccurrenceId, ToolCallId, ToolCallRequest, ToolCallResult};

pub(crate) struct ToolCallOccurrence<'a> {
    pub request: &'a ToolCallRequest,
    pub result: Option<&'a ToolCallResult>,
    pub started: bool,
}

pub(crate) fn tool_call_occurrences(items: &[AgentFrameItem]) -> Vec<ToolCallOccurrence<'_>> {
    let mut calls = Vec::new();
    for item in items {
        match item {
            AgentFrameItem::ToolCallRequested(request) => calls.push(ToolCallOccurrence {
                request,
                result: None,
                started: false,
            }),
            AgentFrameItem::ToolCallStarted(identity) => {
                if let Some(index) =
                    matching_call(&calls, &identity.call_id, &identity.occurrence_id)
                {
                    calls[index].started = true;
                }
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(call_index) =
                    matching_call(&calls, &result.call_id, &result.occurrence_id)
                {
                    calls[call_index].result = Some(result);
                }
            }
            _ => {}
        }
    }
    calls
}

fn matching_call(
    calls: &[ToolCallOccurrence<'_>],
    call_id: &ToolCallId,
    occurrence_id: &OccurrenceId,
) -> Option<usize> {
    calls.iter().rposition(|call| {
        &call.request.call_id == call_id && &call.request.occurrence_id == occurrence_id
    })
}

impl AgentFrame {
    /// Unfinished attempts in request order, including an earlier denial attempt
    /// parked while a retry awaits approval. Recovery must close each tagged
    /// occurrence separately rather than treating one result as closing an id.
    pub fn unfinished_tool_calls(&self) -> Vec<&ToolCallRequest> {
        tool_call_occurrences(&self.items)
            .into_iter()
            .filter(|call| call.result.is_none())
            .map(|call| call.request)
            .collect()
    }
}
