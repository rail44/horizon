//! Assemble provider chunks into live events, replayable history, and a turn
//! outcome. Dropping a failed response never commits its buffered text.

use std::collections::HashMap;

use crossbeam_channel::Sender;
use rig_core::{
    completion::{message::ToolCall, AssistantContent, Message},
    streaming::{StreamedAssistantContent, ToolCallDeltaContent},
};

use crate::{
    config::RigAgentConfig,
    contract::{
        Event, Message as AgentMessage, MessageDelta, MessageRole, ProviderEvent, ToolCallId,
    },
};

use super::super::{
    mapping::{rig_tool_call_provider_payload, rig_tool_call_request},
    StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer,
};
use super::{
    make_tool_call_arguments_replay_safe, output_cap_truncated, partial_assistant_message,
    provider_request_usage_event_from_stream_final, repair_double_encoded_tool_arguments,
    CompletionStop, ToolCallDescriptor, TurnCompletion,
};

pub(super) struct ResponseCollector<'a> {
    events_tx: Sender<ProviderEvent>,
    durable_output_emitted: &'a mut bool,
    first_token_seen: bool,
    text: String,
    requested_tool_call_ids: Vec<ToolCallId>,
    requested_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    max_output_tokens: u64,
    text_buffer: StreamDeltaBuffer,
    reasoning_buffer: StreamDeltaBuffer,
    tool_call_progress: ToolCallProgressBuffer,
}

impl<'a> ResponseCollector<'a> {
    pub(super) fn new(
        config: &RigAgentConfig,
        events_tx: Sender<ProviderEvent>,
        durable_output_emitted: &'a mut bool,
    ) -> Self {
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
            durable_output_emitted,
            first_token_seen: false,
            text: String::new(),
            requested_tool_call_ids: Vec::new(),
            requested_tool_calls: HashMap::new(),
            tool_calls: Vec::new(),
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
        let _ = self.events_tx.send(ProviderEvent::with_provider_payload(
            Event::ToolCallRequested(request),
            provider_payload,
        ));
        self.tool_call_progress.note_finalized(internal_call_id);
        // This must survive a later stream error, which prevents retries.
        *self.durable_output_emitted = true;
        self.tool_calls.push(tool_call);
    }

    pub(super) fn finish(
        mut self,
        cancelled: bool,
        message_id: Option<String>,
        mut content: Vec<AssistantContent>,
    ) -> (Message, TurnCompletion) {
        self.reasoning_buffer.flush();
        self.text_buffer.flush();
        // Tool calls were persisted as they arrived. Text is committed only
        // now, possibly after tool results; replay repairs message grouping.
        if !self.text.is_empty() {
            let _ = self.events_tx.send(
                Event::MessageCommitted(AgentMessage {
                    role: MessageRole::Assistant,
                    text: self.text.clone(),
                })
                .into(),
            );
            *self.durable_output_emitted = true;
        }
        let assistant_message = if cancelled {
            // Rig aggregates its choice only on exhaustion. Cancellation
            // must rebuild history from observed chunks to retain call pairs.
            partial_assistant_message(message_id, &self.text, self.tool_calls)
        } else {
            // Rig's independent aggregate still contains the raw arguments.
            make_tool_call_arguments_replay_safe(&mut content);
            Message::Assistant {
                id: message_id,
                content,
            }
        };
        let truncated_ids = self.tool_call_progress.truncated_ids();
        let cap_truncated =
            output_cap_truncated(self.output_tokens, self.max_output_tokens, cancelled);
        (
            assistant_message,
            TurnCompletion {
                stop: CompletionStop::from_response(
                    cancelled,
                    truncated_ids.len(),
                    cap_truncated,
                    self.text,
                ),
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
