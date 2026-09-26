//! Read-only SDK, compaction and proposer views of canonical conversation.
use super::*;

impl ConversationHistory {
    pub(in crate::providers::rig) fn messages(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        for entry in &self.entries {
            match &entry.content {
                Content::Input { text, .. } => messages.push(Message::user(text.clone())),
                Content::Response(response) => {
                    messages.push(response.message.clone().unwrap_or_else(|| {
                        Message::Assistant {
                            id: None,
                            content: response
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
                        }
                    }));
                    for call in &response.calls {
                        if let Some(result) = &call.result {
                            let mut message = super::super::mapping::rig_tool_result_message(
                                result,
                                &call.tool.function.name,
                            );
                            if let Message::User { content } = &mut message {
                                if let Some(
                                    rig_core::completion::message::UserContent::ToolResult(result),
                                ) = content.first_mut()
                                {
                                    result.call = call.tool.id.clone();
                                    result.provider = call.tool.provider.clone();
                                }
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
    pub(in crate::providers::rig) fn owner_conversation(
        &self,
        include_current: bool,
    ) -> Vec<(bool, String)> {
        let mut output = Vec::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| include_current || entry.turn < self.turn)
        {
            match &entry.content {
                Content::Input {
                    kind: ConversationInputKind::User,
                    text,
                } => output.push((true, text.clone())),
                Content::Response(response)
                    if response.completed
                        && response.calls.is_empty()
                        && self.entries.iter().any(|candidate| {
                            candidate.turn == entry.turn
                                && matches!(
                                    candidate.content,
                                    Content::Input {
                                        kind: ConversationInputKind::User,
                                        ..
                                    }
                                )
                        }) =>
                {
                    if let Some(Message::Assistant { content, .. }) = &response.message {
                        let text: String = content
                            .iter()
                            .filter_map(|c| match c {
                                AssistantContent::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect();
                        if !text.trim().is_empty() && !output.is_empty() {
                            output.push((false, text));
                        }
                    }
                }
                _ => {}
            }
        }
        output
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
