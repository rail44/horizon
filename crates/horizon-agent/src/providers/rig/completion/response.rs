//! Assemble provider chunks into live events, replayable history, and a turn
//! outcome. Text commitment belongs to selection after the retry decision.

use std::collections::{HashMap, HashSet};

use crossbeam_channel::Sender;
use rig_core::{
    completion::{
        message::{AssistantMessage, ToolCall},
        AssistantContent, CompletionResponse, Message,
    },
    streaming::{Item, PartKind, StreamEvent},
};

use crate::{
    config::RigAgentConfig,
    contract::{Event, MessageRole, ProviderEvent, ProviderRequestUsage, ToolCallId},
};

use super::super::{
    mapping::{rig_tool_call_provider_payload, rig_tool_call_request},
    StreamDeltaBuffer, StreamDeltaKind,
};
use super::{
    make_tool_call_arguments_replay_safe, partial_assistant_message,
    repair_double_encoded_tool_arguments, CompletionStop, ToolCallDescriptor, TurnCompletion,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponseEnd {
    Finished,
    Cancelled,
    Failed,
}

pub(super) struct ResponseCollector {
    events_tx: Sender<ProviderEvent>,
    response_id: String,
    first_token_seen: bool,
    text: String,
    requested_tool_call_ids: Vec<ToolCallId>,
    requested_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    tool_calls: Vec<ToolCall>,
    reasoning: Vec<rig_core::completion::message::Reasoning>,
    /// Tool-call parts that opened but never ended. rig 0.44 tells a truncated
    /// stream apart from a complete one by these, not by a final record.
    started_tool_parts: HashSet<usize>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    max_output_tokens: u64,
    text_buffer: StreamDeltaBuffer,
    reasoning_buffer: StreamDeltaBuffer,
}

impl ResponseCollector {
    pub(super) fn new(config: &RigAgentConfig, events_tx: Sender<ProviderEvent>) -> Self {
        Self {
            text_buffer: StreamDeltaBuffer::new(
                events_tx.clone(),
                StreamDeltaKind::AssistantText,
                MessageRole::Assistant,
                config,
            ),
            reasoning_buffer: StreamDeltaBuffer::new(
                events_tx.clone(),
                StreamDeltaKind::Reasoning,
                MessageRole::Assistant,
                config,
            ),
            events_tx,
            response_id: uuid::Uuid::new_v4().to_string(),
            first_token_seen: false,
            text: String::new(),
            requested_tool_call_ids: Vec::new(),
            requested_tool_calls: HashMap::new(),
            tool_calls: Vec::new(),
            reasoning: Vec::new(),
            started_tool_parts: HashSet::new(),
            input_tokens: None,
            output_tokens: None,
            max_output_tokens: config.max_output_tokens,
        }
    }

    /// One item of the reply. Only what the pane shows is kept: text streams as
    /// it arrives, a tool call and a reasoning block are committed when their
    /// part ends. `Start` and `Arguments` carry no meaning the pane uses, so
    /// they are dropped — a tool call appears once, complete.
    pub(super) fn push(&mut self, item: Item<StreamEvent>) {
        let Item::Event(event) = item else {
            return;
        };
        if !self.first_token_seen {
            self.first_token_seen = true;
            let _ = self.events_tx.send(Event::ProviderRequestFirstToken.into());
        }
        match event {
            StreamEvent::Text { text, .. } => {
                self.text.push_str(&text);
                self.text_buffer.push(text);
            }
            // Reasoning stays hidden (decided): the finalized block is kept as
            // history only, never shown, so the deltas are dropped here.
            StreamEvent::Reasoning { .. } => {}
            StreamEvent::Start { part, kind, .. } => {
                if kind == PartKind::ToolCall {
                    self.started_tool_parts.insert(part.index());
                }
            }
            StreamEvent::Arguments { .. } => {}
            StreamEvent::End { part, content } => {
                self.started_tool_parts.remove(&part.index());
                match content {
                    AssistantContent::ToolCall(call) => self.record_tool_call(call),
                    AssistantContent::Reasoning(reasoning) if !reasoning.text.is_empty() => {
                        self.reasoning_buffer.flush();
                        self.reasoning.push(reasoning);
                    }
                    _ => {}
                }
            }
        }
    }

    fn record_tool_call(&mut self, mut tool_call: ToolCall) {
        self.reasoning_buffer.flush();
        self.text_buffer.flush();
        // Keep the raw emission for forensics; repair only execution and replay.
        let provider_payload = rig_tool_call_provider_payload(&tool_call);
        let mut arguments = serde_json::Value::Object(tool_call.function.arguments.clone());
        repair_double_encoded_tool_arguments(&mut arguments);
        if let serde_json::Value::Object(map) = arguments {
            tool_call.function.arguments = map;
        }
        let request = rig_tool_call_request(tool_call.clone());
        self.requested_tool_call_ids.push(request.call_id.clone());
        self.requested_tool_calls.insert(
            request.call_id.clone(),
            ToolCallDescriptor {
                identity: request.identity(),
                tool_id: request.tool_id.clone(),
                args: request.input.0.clone(),
            },
        );
        let _ = self.events_tx.send(
            Event::ConversationRecorded(crate::contract::ConversationRecord::ToolAnnounced {
                response_id: self.response_id.clone(),
                identity: request.identity(),
                codec: super::super::conversation::MESSAGE_CODEC,
                tool_call: serde_json::to_value(&tool_call)
                    .expect("tool call serializes")
                    .into(),
                reasoning: serde_json::to_value(&self.reasoning)
                    .expect("reasoning serializes")
                    .into(),
            })
            .into(),
        );
        let _ = self.events_tx.send(ProviderEvent::with_provider_payload(
            Event::ToolCallRequested(request),
            provider_payload,
        ));
        self.tool_calls.push(tool_call);
    }

    pub(super) fn has_issued_tools(&self) -> bool {
        !self.requested_tool_calls.is_empty()
    }

    pub(super) fn finish(
        mut self,
        end: ResponseEnd,
        message_id: Option<String>,
        response: Option<CompletionResponse>,
    ) -> (Message, TurnCompletion) {
        self.reasoning_buffer.flush();
        self.text_buffer.flush();

        let had_response = response.is_some();
        let mut finish_reason = None;
        if let Some(response) = &response {
            self.record_usage(response);
            finish_reason = response.finish_reason();
            if let Some(id) = response.identity().response_id {
                self.response_id = id;
            }
        }

        let assistant_message = match (end, response) {
            (ResponseEnd::Finished, Some(response)) => {
                let mut content = response.choice;
                // Rig's independent aggregate still contains the raw arguments.
                make_tool_call_arguments_replay_safe(&mut content);
                Message::Assistant(AssistantMessage::new(content))
            }
            // Cancellation and failure: a reply folded only on exhaustion leaves
            // no choice, so history is rebuilt from the observed parts to keep
            // every call/result pair.
            _ => {
                let mut message =
                    partial_assistant_message(message_id, &self.text, self.tool_calls);
                if let Message::Assistant(assistant) = &mut message {
                    assistant.content.splice(
                        0..0,
                        self.reasoning.into_iter().map(AssistantContent::Reasoning),
                    );
                }
                message
            }
        };

        let stop = match end {
            ResponseEnd::Cancelled => CompletionStop::Cancelled,
            ResponseEnd::Failed => CompletionStop::Failed,
            ResponseEnd::Finished if !had_response => CompletionStop::Unknown {
                reason: "stream ended without a final record".into(),
            },
            ResponseEnd::Finished => CompletionStop::from_provider(
                finish_reason,
                self.started_tool_parts.len(),
                self.output_tokens,
                self.max_output_tokens,
                self.text,
            ),
        };
        (
            assistant_message,
            TurnCompletion {
                stop,
                response_id: self.response_id,
                requested_tool_call_ids: self.requested_tool_call_ids,
                requested_tool_calls: self.requested_tool_calls,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            },
        )
    }

    fn record_usage(&mut self, response: &CompletionResponse) {
        let usage = &response.usage;
        // Unreported is not zero: no event at all rather than a fabricated one.
        if !usage.is_reported() {
            return;
        }
        let input_tokens = usage.input_tokens.unwrap_or(0);
        let output_tokens = usage.output_tokens.unwrap_or(0);
        let total_tokens = usage.total_tokens.unwrap_or(input_tokens + output_tokens);
        let cached_input_tokens = usage.cached_input_tokens.unwrap_or(0);
        self.input_tokens = Some(input_tokens);
        self.output_tokens = Some(output_tokens);
        let _ = self.events_tx.send(
            Event::ProviderRequestUsage(ProviderRequestUsage {
                input_tokens,
                output_tokens,
                total_tokens,
                cached_input_tokens,
            })
            .into(),
        );
    }
}

#[cfg(test)]
mod tests;
