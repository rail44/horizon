//! Restore provider calls from execution attempts. Retries remain separate in
//! the event log, but the provider waits for one final answer to its request.

use std::collections::{HashMap, HashSet};

use crate::contract::{OccurrenceId, ToolCallId, ToolCallRequest, ToolCallResult};
use rig_core::completion::Message;

use super::{rig_tool_call_from_request, rig_tool_result_message};

#[derive(Default)]
pub(super) struct ReplayedToolCalls<'a> {
    calls: Vec<ProviderCall<'a>>,
    occurrences: HashMap<&'a OccurrenceId, usize>,
    retired: HashSet<OccurrenceId>,
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
        self.occurrences.insert(&request.occurrence_id, index);
        retry
            .is_none()
            .then(|| Message::from(rig_tool_call_from_request(request)))
    }

    pub(super) fn result(&mut self, result: &ToolCallResult) -> Option<Message> {
        if result.is_superseded() {
            self.retired.insert(result.occurrence_id.clone());
            return None;
        }
        if self.retired.contains(&result.occurrence_id) {
            return None;
        }
        let index = self.occurrences.get(&result.occurrence_id).copied();
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
