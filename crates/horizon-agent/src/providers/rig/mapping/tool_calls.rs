//! Restore provider calls from execution attempts. Retries remain separate in
//! the event log, but the provider waits for one final answer to its request.

use std::collections::HashMap;

use crate::contract::{
    is_superseded_output, OccurrenceId, ToolCallId, ToolCallRequest, ToolCallResult,
};
use rig_core::completion::Message;

use super::{rig_tool_call_from_request, rig_tool_result_message};

#[derive(Default)]
pub(super) struct ReplayedToolCalls<'a> {
    calls: Vec<ProviderCall<'a>>,
    occurrences: HashMap<&'a OccurrenceId, usize>,
    latest: HashMap<&'a ToolCallId, usize>,
    pending: HashMap<&'a ToolCallId, usize>,
}

struct ProviderCall<'a> {
    request: &'a ToolCallRequest,
    answered: bool,
}

impl<'a> ReplayedToolCalls<'a> {
    pub(super) fn request(&mut self, request: &'a ToolCallRequest) -> Option<Message> {
        // The live provider has one pending entry per call id. Reissuing the
        // same input while it waits creates an execution attempt, not another
        // provider call. A settled call or a new turn can reuse that id freely.
        let retry = self
            .pending
            .get(&request.call_id)
            .copied()
            .filter(|&index| {
                let prior = self.calls[index].request;
                prior.tool_id == request.tool_id && prior.input == request.input
            });
        let index = retry.unwrap_or_else(|| {
            let index = self.calls.len();
            self.calls.push(ProviderCall {
                request,
                answered: false,
            });
            self.pending.insert(&request.call_id, index);
            index
        });
        self.latest.insert(&request.call_id, index);
        if let Some(occurrence) = &request.occurrence_id {
            self.occurrences.insert(occurrence, index);
        }
        retry
            .is_none()
            .then(|| Message::from(rig_tool_call_from_request(request)))
    }

    pub(super) fn result(&mut self, result: &ToolCallResult) -> Option<Message> {
        if is_superseded_output(&result.output) {
            return None;
        }
        // Tagged results never fall back to an unrelated use of the same id.
        // Older events without occurrence tags bind to the preceding request.
        let index = match &result.occurrence_id {
            Some(occurrence) => self.occurrences.get(occurrence),
            None => self.latest.get(&result.call_id),
        }
        .copied();
        let Some(index) = index else {
            eprintln!(
                "horizon-agent: dropped unmatched tool result {} while rebuilding provider history",
                result.call_id.0
            );
            return None;
        };
        let call = &mut self.calls[index];
        if call.answered || call.request.call_id != result.call_id {
            eprintln!("horizon-agent: dropped duplicate or mismatched tool result {} while rebuilding provider history", result.call_id.0);
            return None;
        }
        call.answered = true;
        if self.pending.get(&result.call_id) == Some(&index) {
            self.pending.remove(&result.call_id);
        }
        Some(rig_tool_result_message(result, &call.request.tool_id))
    }

    pub(super) fn start_turn(&mut self) {
        // A new provider request/user turn must not inherit retry identity from
        // an interrupted turn. Keep occurrence/name evidence for late results.
        self.pending.clear();
    }
}
