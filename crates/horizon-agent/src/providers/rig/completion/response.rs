//! Assemble provider chunks into live events, replayable history, and a turn
//! outcome. Text commitment belongs to selection after the retry decision.

use std::collections::HashMap;

use crossbeam_channel::Sender;
use rig_core::{
    completion::{message::ToolCall, AssistantContent, Message},
    streaming::{StreamedAssistantContent, ToolCallDeltaContent},
};

use crate::{
    config::RigAgentConfig,
    contract::{Event, MessageDelta, MessageRole, ProviderEvent, ToolCallId},
};

use super::super::{
    mapping::{rig_tool_call_provider_payload, rig_tool_call_request},
    StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer,
};
use super::{
    make_tool_call_arguments_replay_safe, partial_assistant_message,
    provider_request_usage_event_from_stream_final, repair_double_encoded_tool_arguments,
    CompletionStop, ToolCallDescriptor, TurnCompletion,
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
    finish_reason: Option<rig_core::completion::FinishReason>,
    final_seen: bool,
    first_token_seen: bool,
    text: String,
    requested_tool_call_ids: Vec<ToolCallId>,
    requested_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    tool_calls: Vec<ToolCall>,
    reasoning: Vec<rig_core::completion::message::Reasoning>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    max_output_tokens: u64,
    text_buffer: StreamDeltaBuffer,
    reasoning_buffer: StreamDeltaBuffer,
    tool_call_progress: ToolCallProgressBuffer,
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
            tool_call_progress: ToolCallProgressBuffer::new(events_tx.clone(), config),
            events_tx,
            response_id: uuid::Uuid::new_v4().to_string(),
            finish_reason: None,
            final_seen: false,
            first_token_seen: false,
            text: String::new(),
            requested_tool_call_ids: Vec::new(),
            requested_tool_calls: HashMap::new(),
            tool_calls: Vec::new(),
            reasoning: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            max_output_tokens: config.max_output_tokens,
        }
    }

    pub(super) fn push(&mut self, chunk: StreamedAssistantContent) {
        if !self.first_token_seen {
            self.first_token_seen = true;
            let _ = self.events_tx.send(Event::ProviderRequestFirstToken.into());
        }
        match chunk {
            StreamedAssistantContent::Text(delta) => {
                self.text.push_str(&delta.text);
                self.text_buffer.push(delta.text);
            }
            StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                self.reasoning_buffer.push(reasoning);
            }
            StreamedAssistantContent::Reasoning { reasoning, .. } => {
                self.reasoning_buffer.flush();
                self.reasoning.push(reasoning.clone());
                let text = reasoning.display_text();
                if !text.is_empty() {
                    let _ = self.events_tx.send(
                        Event::ReasoningDelta(MessageDelta {
                            role: MessageRole::Assistant,
                            text,
                        })
                        .into(),
                    );
                }
            }
            StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id,
            } => {
                self.record_tool_call(tool_call, &internal_call_id);
            }
            StreamedAssistantContent::ToolCallDelta {
                internal_call_id,
                content,
            } => match content {
                ToolCallDeltaContent::Name(name) => {
                    self.tool_call_progress.note_name(&internal_call_id, name)
                }
                ToolCallDeltaContent::Delta(delta) => self
                    .tool_call_progress
                    .note_delta(&internal_call_id, &delta),
            },
            StreamedAssistantContent::Final(response) => {
                self.final_seen = true;
                self.finish_reason = response.finish_reason.clone();
                let usage = provider_request_usage_event_from_stream_final(&response);
                if let Event::ProviderRequestUsage(usage) = &usage {
                    self.input_tokens = Some(usage.input_tokens);
                    self.output_tokens = Some(usage.output_tokens);
                }
                let _ = self.events_tx.send(usage.into());
            }
            StreamedAssistantContent::Unknown(_) => {}
        }
    }

    fn record_tool_call(&mut self, mut tool_call: ToolCall, internal_call_id: &str) {
        self.reasoning_buffer.flush();
        self.text_buffer.flush();
        if !self.tool_call_progress.note_finalized(internal_call_id) {
            return;
        }
        // Keep the raw emission for forensics; repair only execution and replay.
        let provider_payload = rig_tool_call_provider_payload(&tool_call);
        repair_double_encoded_tool_arguments(&mut tool_call.function.arguments);
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
        mut content: Vec<AssistantContent>,
    ) -> (Message, TurnCompletion) {
        self.reasoning_buffer.flush();
        self.text_buffer.flush();
        let assistant_message = if end != ResponseEnd::Finished {
            // Rig aggregates its choice only on exhaustion. Cancellation
            // must rebuild history from observed chunks to retain call pairs.
            let mut message = partial_assistant_message(message_id, &self.text, self.tool_calls);
            if let Message::Assistant { content, .. } = &mut message {
                content.splice(
                    0..0,
                    self.reasoning.into_iter().map(AssistantContent::Reasoning),
                );
            }
            message
        } else {
            // Rig's independent aggregate still contains the raw arguments.
            make_tool_call_arguments_replay_safe(&mut content);
            Message::Assistant {
                id: message_id,
                content,
            }
        };
        let stop = match end {
            ResponseEnd::Cancelled => CompletionStop::Cancelled,
            ResponseEnd::Failed => CompletionStop::Failed,
            ResponseEnd::Finished if !self.final_seen => CompletionStop::Unknown {
                reason: "stream ended without a final record".into(),
            },
            ResponseEnd::Finished => CompletionStop::from_provider(
                self.finish_reason,
                self.tool_call_progress.truncated_ids().len(),
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
}

#[cfg(test)]
mod tests;
