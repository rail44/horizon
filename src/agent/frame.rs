use crate::agent::contract::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentFrame {
    pub(crate) state: Option<SessionState>,
    pub(crate) items: Vec<AgentFrameItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AgentFrameItem {
    Message(Message),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallId),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    Error(Error),
    Exited(Exit),
}

impl AgentFrame {
    pub(crate) fn empty() -> Self {
        Self {
            state: None,
            items: Vec::new(),
        }
    }

    pub(crate) fn pending_approval_call_id(&self) -> Option<ToolCallId> {
        let mut pending = Vec::<ToolCallId>::new();
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

    /// The most recent `ToolCallRequested` item for `call_id`, if any. Used
    /// to recover a pending tool call's `tool_id`/`input` at approval time,
    /// since the approve/deny UI only carries the `call_id` forward.
    pub(crate) fn tool_call_request(&self, call_id: &ToolCallId) -> Option<&ToolCallRequest> {
        self.items.iter().rev().find_map(|item| match item {
            AgentFrameItem::ToolCallRequested(request) if &request.call_id == call_id => {
                Some(request)
            }
            _ => None,
        })
    }

    /// Whether a turn is currently in flight (streaming, running a tool, or
    /// waiting on tool-call approval) and therefore cancellable.
    pub(crate) fn is_turn_in_flight(&self) -> bool {
        matches!(
            self.state,
            Some(
                SessionState::Running
                    | SessionState::WaitingForApproval
                    | SessionState::ToolRunning
            )
        )
    }

    /// Whether `call_id` already has a terminal `ToolCallFinished` in the
    /// frame — from an earlier approve/deny short-circuit, a genuine result,
    /// or a cancellation that finished the call. Used to guard against
    /// double-folding a late result that arrives after the call already
    /// resolved: `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved`
    /// check, and the bash tool's async completion delivery
    /// (`app/runtime/agent.rs`), both key off this.
    pub(crate) fn has_tool_call_finished(&self, call_id: &ToolCallId) -> bool {
        self.items.iter().any(|item| {
            matches!(item, AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id)
        })
    }
}

#[cfg(test)]
pub(crate) fn render_agent_transcript(events: &[Event]) -> String {
    let mut lines = vec!["Agent session".to_string(), String::new()];

    for event in events {
        match event {
            Event::StateChanged(state) => lines.push(format!("state: {state:?}")),
            Event::ReasoningDelta(delta) => {
                lines.push(format!("{}: {}", role_label(delta.role), delta.text));
            }
            Event::AssistantTextDelta(delta) => {
                lines.push(format!("{} delta: {}", role_label(delta.role), delta.text));
            }
            Event::MessageCommitted(message) => {
                lines.push(format!("{}: {}", role_label(message.role), message.text));
            }
            Event::ToolCallRequested(request) => {
                lines.push(format!(
                    "tool requested: {} ({})",
                    request.tool_id, request.call_id.0
                ));
            }
            Event::ToolCallStarted(call_id) => {
                lines.push(format!("tool started: {}", call_id.0));
            }
            Event::ToolCallFinished(result) => {
                lines.push(format!(
                    "tool finished: {} {}",
                    result.call_id.0, result.output
                ));
            }
            Event::ApprovalRequested(request) => {
                lines.push(format!(
                    "approval requested: {} {}",
                    request.call_id.0, request.reason
                ));
            }
            Event::Error(error) => lines.push(format!("error: {}", error.message)),
            Event::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
        }
    }

    lines.join("\n")
}

pub(crate) fn agent_frame_from_events(events: &[Event]) -> AgentFrame {
    let mut frame = AgentFrame::empty();

    for event in events {
        apply_agent_event_to_frame(&mut frame, event);
    }

    frame
}

pub(crate) fn apply_agent_event_to_frame(frame: &mut AgentFrame, event: &Event) {
    match event {
        Event::StateChanged(state) => frame.state = Some(*state),
        Event::ReasoningDelta(delta) => {
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
        Event::AssistantTextDelta(delta) => {
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
        Event::MessageCommitted(message) => {
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
        Event::ToolCallRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallRequested(request.clone()));
        }
        Event::ToolCallStarted(call_id) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallStarted(call_id.clone()));
        }
        Event::ToolCallFinished(result) => {
            frame
                .items
                .push(AgentFrameItem::ToolCallFinished(result.clone()));
        }
        Event::ApprovalRequested(request) => {
            frame
                .items
                .push(AgentFrameItem::ApprovalRequested(request.clone()));
        }
        Event::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        Event::Exited(exit) => frame.items.push(AgentFrameItem::Exited(exit.clone())),
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
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            ..
        }) | AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::Error(_)
            | AgentFrameItem::Exited(_)
    )
}

#[cfg(test)]
fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
