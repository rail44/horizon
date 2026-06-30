use super::types::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentFrame {
    pub state: Option<AgentSessionState>,
    pub items: Vec<AgentFrameItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentFrameItem {
    Message(AgentMessage),
    ReasoningDelta(AgentMessageDelta),
    AssistantTextDelta(AgentMessageDelta),
    ToolCallRequested(AgentToolCallRequest),
    ToolCallStarted(AgentToolCallId),
    ToolCallFinished(AgentToolCallResult),
    ApprovalRequested(AgentApprovalRequest),
    Error(AgentError),
    Exited(AgentExit),
}

impl AgentFrame {
    pub fn empty() -> Self {
        Self {
            state: None,
            items: Vec::new(),
        }
    }

    pub fn pending_approval_call_id(&self) -> Option<AgentToolCallId> {
        let mut pending = Vec::<AgentToolCallId>::new();
        for item in &self.items {
            match item {
                AgentFrameItem::ApprovalRequested(request) => {
                    if !pending.contains(&request.call_id) {
                        pending.push(request.call_id.clone());
                    }
                }
                AgentFrameItem::ToolCallFinished(result) => {
                    pending.retain(|call_id| call_id != &result.call_id);
                }
                _ => {}
            }
        }

        pending.last().cloned()
    }
}

pub fn render_agent_transcript(events: &[AgentEvent]) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];

    for event in events {
        match event {
            AgentEvent::StateChanged(state) => lines.push(format!("state: {state:?}")),
            AgentEvent::ReasoningDelta(delta) => {
                lines.push(format!("{}: {}", role_label(delta.role), delta.text));
            }
            AgentEvent::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", role_label(delta.role), delta.text));
            }
            AgentEvent::MessageCommitted(message) => {
                lines.push(format!("{}: {}", role_label(message.role), message.text));
            }
            AgentEvent::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            AgentEvent::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            AgentEvent::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output
                ));
            }
            AgentEvent::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            AgentEvent::Error(error) => lines.push(format!("error: {}", error.message)),
            AgentEvent::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
        }
    }

    lines.join("\n")
}

pub fn agent_frame_from_events(events: &[AgentEvent]) -> AgentFrame {
    let mut frame = AgentFrame::empty();

    for event in events {
        apply_agent_event_to_frame(&mut frame, event);
    }

    frame
}

pub(crate) fn apply_agent_event_to_frame(frame: &mut AgentFrame, event: &AgentEvent) {
    match event {
        AgentEvent::StateChanged(state) => frame.state = Some(*state),
        AgentEvent::ReasoningDelta(delta) => {
            if let Some(AgentFrameItem::ReasoningDelta(existing)) =
                last_current_turn_item_mut(frame, |item| {
                    matches!(item, AgentFrameItem::ReasoningDelta(_))
                })
            {
                if existing.role == delta.role {
                    existing.text.push_str(&delta.text);
                    return;
                }
            }
            frame
                .items
                .push(AgentFrameItem::ReasoningDelta(delta.clone()));
        }
        AgentEvent::AssistantTextDelta(delta) => {
            if let Some(AgentFrameItem::AssistantTextDelta(existing)) =
                last_current_turn_item_mut(frame, |item| {
                    matches!(item, AgentFrameItem::AssistantTextDelta(_))
                })
            {
                if existing.role == delta.role {
                    existing.text.push_str(&delta.text);
                    return;
                }
            }
            frame
                .items
                .push(AgentFrameItem::AssistantTextDelta(delta.clone()));
        }
        AgentEvent::MessageCommitted(message) => {
            if let Some(index) = last_current_turn_item_index(frame, |item| {
                matches!(item, AgentFrameItem::AssistantTextDelta(_))
            }) {
                if let AgentFrameItem::AssistantTextDelta(existing) = &frame.items[index] {
                    if existing.role == message.role {
                        frame.items[index] = AgentFrameItem::Message(message.clone());
                        return;
                    }
                }
            }
            if let Some(index) = last_current_turn_item_index(frame, |item| {
                matches!(item, AgentFrameItem::Message(_))
            }) {
                if let AgentFrameItem::Message(existing) = &frame.items[index] {
                    if existing.role == message.role {
                        frame.items[index] = AgentFrameItem::Message(message.clone());
                        return;
                    }
                }
            }
            frame.items.push(AgentFrameItem::Message(message.clone()));
        }
        AgentEvent::ToolCallRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallRequested(request.clone()));
        }
        AgentEvent::ToolCallStarted(call_id) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallStarted(call_id.clone()));
        }
        AgentEvent::ToolCallFinished(result) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallFinished(result.clone()));
        }
        AgentEvent::ApprovalRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ApprovalRequested(request.clone()));
        }
        AgentEvent::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        AgentEvent::Exited(exit) => frame.items.push(AgentFrameItem::Exited(exit.clone())),
    }
}

fn last_current_turn_item_mut(
    frame: &mut AgentFrame,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<&mut AgentFrameItem> {
    let index = last_current_turn_item_index(frame, predicate)?;
    frame.items.get_mut(index)
}

fn last_current_turn_item_index(
    frame: &AgentFrame,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<usize> {
    let start = frame
        .items
        .iter()
        .rposition(is_turn_boundary_item)
        .map_or(0, |index| index + 1);

    frame.items[start..]
        .iter()
        .rposition(predicate)
        .map(|index| start + index)
}

fn is_turn_boundary_item(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(AgentMessage {
            role: AgentMessageRole::User,
            ..
        }) | AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::Error(_)
            | AgentFrameItem::Exited(_)
    )
}

pub fn render_agent_transcript_from_frame(frame: &AgentFrame) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];
    if let Some(state) = frame.state {
        lines.push(format!("state: {state:?}"));
    }

    for item in &frame.items {
        match item {
            AgentFrameItem::Message(message) => {
                lines.push(format!("{}: {}", role_label(message.role), message.text));
            }
            AgentFrameItem::ReasoningDelta(delta) => {
                lines.push(format!(
                    "{} reasoning: {}",
                    role_label(delta.role),
                    delta.text
                ));
            }
            AgentFrameItem::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", role_label(delta.role), delta.text));
            }
            AgentFrameItem::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            AgentFrameItem::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output
                ));
            }
            AgentFrameItem::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            AgentFrameItem::Error(error) => lines.push(format!("error: {}", error.message)),
            AgentFrameItem::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
        }
    }

    lines.join("\n")
}

fn role_label(role: AgentMessageRole) -> &'static str {
    match role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    }
}
