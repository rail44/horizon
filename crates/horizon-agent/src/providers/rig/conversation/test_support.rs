use super::*;

pub(crate) fn fixture(messages: Vec<Message>) -> ConversationHistory {
    let (events, _) = crossbeam_channel::unbounded();
    let mut history = ConversationHistory::default();
    for (index, message) in messages.into_iter().enumerate() {
        match message {
            Message::User { content } => {
                history.open_turn(&events);
                for item in content {
                    let rig_core::completion::message::UserContent::Text(text) = item else {
                        panic!("fixture input must be text")
                    };
                    history
                        .append_prompt(
                            Prompt::input(ConversationInputKind::User, text.text),
                            &events,
                        )
                        .unwrap();
                }
            }
            Message::Assistant { ref content, .. } => {
                let calls = content
                    .iter()
                    .filter_map(|part| match part {
                        AssistantContent::ToolCall(tool) => Some(ToolCallIdentity {
                            call_id: ToolCallId(provider_call_id(tool).into()),
                            occurrence_id: OccurrenceId(provider_call_id(tool).into()),
                        }),
                        _ => None,
                    })
                    .collect();
                history
                    .record_response(format!("fixture-{index}"), &message, calls, &events)
                    .unwrap();
                history
                    .apply_event(&Event::TurnEnded(crate::contract::TurnEndReason::Completed))
                    .unwrap();
            }
            _ => panic!("unsupported fixture message"),
        }
    }
    history
}

pub(crate) fn announcement(request: &crate::contract::ToolCallRequest, response: &str) -> Event {
    Event::ConversationRecorded(ConversationRecord::ToolAnnounced {
        response_id: response.into(),
        identity: request.identity(),
        codec: MESSAGE_CODEC,
        tool_call: serde_json::to_value(super::super::mapping::rig_tool_call_from_request(request))
            .unwrap()
            .into(),
        reasoning: serde_json::json!([]).into(),
    })
}
