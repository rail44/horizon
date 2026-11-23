//! Read-only SDK and compaction views of canonical conversation.
use super::*;

/// What a request carries of the turns before the one in progress: their
/// reasoning, or not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::providers::rig) enum EarlierReasoning {
    /// Sent back as it came, for rig and the provider to handle: Anthropic's
    /// API drops earlier turns' thinking on its side, and OpenAI's
    /// Responses API pairs a call item with the reasoning item it followed.
    Kept,
    /// Left out: an OpenAI-compatible chat-completions endpoint counts
    /// whatever is sent as input, and on the owner's 2026-10-20 session
    /// reasoning was about a third of a 620k-token input
    /// (`docs/agent-compaction-design.md`, the note of that date).
    Dropped,
}

impl ConversationHistory {
    /// The history as rig takes it, earlier turns' reasoning left out.
    #[cfg(test)]
    pub(in crate::providers::rig) fn messages(&self) -> Vec<Message> {
        self.messages_with(EarlierReasoning::Dropped)
    }

    /// The history as rig takes it, carrying `earlier` of the turns before
    /// the one in progress. The turn in progress always keeps its
    /// reasoning: a model that reasons before it answers needs its current
    /// tool-call round's reasoning back (DeepSeek's own API refuses a round
    /// without it).
    pub(in crate::providers::rig) fn messages_with(
        &self,
        earlier: EarlierReasoning,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        for entry in &self.entries {
            match &entry.content {
                Content::Input { text, .. } => messages.push(Message::user(text.clone())),
                Content::Response(response) => {
                    let keeps = entry.turn == self.turn || earlier == EarlierReasoning::Kept;
                    messages.push(assistant_message(response, keeps));
                    for call in &response.calls {
                        if let Some(result) = &call.result {
                            let mut message = super::super::mapping::rig_tool_result_message(
                                result,
                                &call.tool.function.name,
                            );
                            if let Message::User { content } = &mut message
                                && let Some(rig_core::completion::message::UserContent::ToolResult(
                                    result,
                                )) = content.first_mut()
                            {
                                result.call = call.tool.id.clone();
                            }
                            messages.push(message);
                        }
                    }
                }
            }
        }
        messages
    }
    pub(in crate::providers::rig) fn len(&self) -> usize {
        self.entries.iter().map(entry_len).sum()
    }
}

/// The assistant message a response projects to, with its reasoning when
/// `keeps_reasoning` ([`EarlierReasoning`]).
fn assistant_message(response: &Response, keeps_reasoning: bool) -> Message {
    let mut message = match &response.message {
        Some(Message::Assistant(message)) => message.clone(),
        Some(other) => return other.clone(),
        None => rig_core::completion::message::AssistantMessage::new(
            response
                .reasoning
                .iter()
                .cloned()
                .map(AssistantContent::Reasoning)
                .chain(
                    response
                        .calls
                        .iter()
                        .map(|call| AssistantContent::ToolCall(call.tool.clone())),
                )
                .collect(),
        ),
    };
    if !keeps_reasoning {
        message
            .content
            .retain(|part| !matches!(part, AssistantContent::Reasoning(_)));
        if message.content.is_empty() {
            // A reply that was reasoning alone still has to stand as a turn.
            message.content.push(AssistantContent::text(""));
        }
    }
    Message::Assistant(message)
}

impl ConversationHistory {
    pub(in crate::providers::rig) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub(in crate::providers::rig) fn turn_start(&self) -> usize {
        self.entries
            .iter()
            .take_while(|entry| entry.turn != self.turn)
            .map(entry_len)
            .sum()
    }
    pub(in crate::providers::rig) fn result_sites(&self) -> Vec<ResultSite<'_>> {
        let latest = self
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.content {
                Content::Response(response) if !response.calls.is_empty() => {
                    Some(response.id.as_str())
                }
                _ => None,
            });
        let mut index = 0;
        let mut sites = Vec::new();
        for entry in &self.entries {
            index += 1;
            if let Content::Response(response) = &entry.content {
                for call in &response.calls {
                    if let Some(result) = &call.result {
                        sites.push(ResultSite {
                            result,
                            tool: &call.tool,
                            message_index: index,
                            current_response: Some(response.id.as_str()) == latest,
                        });
                        index += 1;
                    }
                }
            }
        }
        sites
    }
}
fn entry_len(entry: &Entry) -> usize {
    match &entry.content {
        Content::Input { .. } => 1,
        Content::Response(response) => {
            1 + response
                .calls
                .iter()
                .filter(|call| call.result.is_some())
                .count()
        }
    }
}
