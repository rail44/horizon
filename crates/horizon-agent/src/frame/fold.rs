use crate::contract::*;

use super::types::{AgentFrame, AgentFrameItem};

/// Reducer-side bookkeeping threaded through [`apply_agent_event_to_frame`]:
/// where the current provider response's items start in the frame, so a
/// retried request retires only its own live preview and a commit promotes
/// only its own streamed deltas. Kept beside `AgentFrame` rather than on
/// it, like [`StateEntry`], so frames compare without it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TurnClock {
    response_start: usize,
}

impl TurnClock {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

pub fn agent_frame_from_events(events: &[Event]) -> AgentFrame {
    agent_frame_and_turn_clock_from_events(events).0
}

/// [`agent_frame_from_events`]'s full computation, also returning the
/// [`TurnClock`] the replay ended with -- `live::State::from_history` uses
/// this (rather than the frame-only wrapper) so a resumed session's live
/// fold continues from the same turn bookkeeping a continuously-running
/// session would have had, instead of restarting it from scratch.
pub(crate) fn agent_frame_and_turn_clock_from_events(events: &[Event]) -> (AgentFrame, TurnClock) {
    let mut frame = AgentFrame::empty();
    let mut turn = TurnClock::new();

    for event in events {
        apply_agent_event_to_frame(&mut frame, event, &mut turn);
    }

    (frame, turn)
}

pub(crate) fn apply_agent_event_to_frame(
    frame: &mut AgentFrame,
    event: &Event,
    turn: &mut TurnClock,
) {
    match event {
        Event::StateChanged(state) => {
            frame.state = Some(*state);
            frame.turn_end_reason = match state {
                SessionState::Running
                | SessionState::ToolRunning
                | SessionState::WaitingForApproval => None,
                SessionState::Failed => Some(TurnEndReason::Failed),
                SessionState::Cancelled => Some(TurnEndReason::Cancelled),
                SessionState::Created
                | SessionState::WaitingForUser
                | SessionState::Terminated
                | SessionState::Suspended => frame.turn_end_reason,
            };
        }
        // Streamed text and reasoning reach a client through its own fold of
        // the events; the frame keeps what was committed.
        Event::ReasoningDelta(_) | Event::AssistantTextDelta(_) => {}
        Event::MessageCommitted(message) => {
            // A message committed again in the same response replaces the
            // one before it; any other is added.
            if let Some(index) =
                last_current_turn_item_index(frame, is_turn_boundary_item, |item| {
                    matches!(item, AgentFrameItem::Message(_))
                })
                && index >= turn.response_start
                && let AgentFrameItem::Message(existing) = &frame.items[index]
                && existing.role == message.role
            {
                frame.items[index] = AgentFrameItem::Message(message.clone());
                return;
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
        // Request markers add no transcript rows; a request marks where its
        // response begins.
        Event::ProviderRequestSent(_) => {
            turn.response_start = frame.items.len();
        }
        Event::ProviderRequestFinished
        | Event::ProviderRequestFirstToken
        | Event::ProviderRequestUsage(_) => {}
        Event::ProviderRateLimited(rate_limited) => {
            frame
                .items
                .push(AgentFrameItem::ProviderRateLimited(rate_limited.clone()));
        }
        Event::HistoryCleared(cleared) => {
            frame
                .items
                .push(AgentFrameItem::HistoryCleared(cleared.clone()));
        }
        // Audit/observability-only (board #41): no frame item.
        Event::SessionInputSent { .. }
        | Event::EnvironmentReady { .. }
        | Event::EnvironmentActivated(_)
        | Event::EnvironmentActivationFailed(_)
        | Event::SessionResumed
        | Event::Rewound { .. }
        | Event::ModelSelected { .. }
        | Event::ReasoningSelected { .. }
        | Event::InputQueuePaused(_)
        | Event::InputStarted(_)
        | Event::InputAccepted(_)
        | Event::InputOutcome(_)
        | Event::DeliveryAcknowledged(_)
        | Event::ConversationRecorded(_) => {}
        Event::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        Event::Exited(exit) => {
            frame.state = Some(SessionState::Terminated);
            frame.items.push(AgentFrameItem::Exited(exit.clone()));
        }
        Event::ApprovalResolved(resolved) => {
            frame
                .items
                .push(AgentFrameItem::ApprovalResolved(resolved.clone()));
        }
        Event::ContinueTurnRequested(_) | Event::ContinuationVerdict(_) => {}
        Event::TurnEnded(reason) => {
            frame.turn_end_reason = Some(*reason);
            frame
                .items
                .push(AgentFrameItem::TurnEnded { reason: *reason });
        }
    }
}

/// Finds the last item matching `predicate` in the current turn, where the
/// turn's extent is defined by `boundary`: the search starts just after the
/// last item for which `boundary` returns true (or from the beginning if
/// none). The boundary is a parameter so different folds can scope their
/// lookups differently -- see [`is_turn_boundary_item`] (strict, used for
/// a re-committed message) and [`is_turn_opening_boundary_item`] (loose,
/// used for a tool call's preview).
fn last_current_turn_item_index(
    frame: &AgentFrame,
    boundary: impl Fn(&AgentFrameItem) -> bool,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<usize> {
    let start = frame
        .items
        .iter()
        .rposition(boundary)
        .map_or(0, |index| index + 1);

    frame.items[start..]
        .iter()
        .rposition(predicate)
        .map(|index| start + index)
}

/// The **strict** turn boundary: every item type that closes the current
/// message/tool-call run. Compare [`is_turn_opening_boundary_item`], the
/// **loose** boundary, which excludes the tool-call and approval items.
fn is_turn_boundary_item(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(Message {
            // A `task` notification is an input to the model just like a
            // user message, so it closes whatever delta/message run
            // preceded it rather than merging into it.
            role: MessageRole::User | MessageRole::TaskNotification,
            ..
        }) | AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::ApprovalResolved(_)
            | AgentFrameItem::Error(_)
            | AgentFrameItem::Exited(_)
            | AgentFrameItem::TurnEnded { .. }
    )
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn failed_attempt_text_is_retired_before_the_next_response_and_replay_agrees() {
        let live = crate::live::LiveState::with_disabled_persistence();
        let events = vec![
            Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "question".into(),
            }),
            Event::ProviderRequestSent(ProviderRequestSent {
                model: "model".into(),
            }),
            Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "failed prefix".into(),
            }),
            Event::ProviderRequestFinished,
            Event::ProviderRequestSent(ProviderRequestSent {
                model: "model".into(),
            }),
            Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "new answer".into(),
            }),
            Event::ProviderRequestFinished,
            Event::MessageCommitted(Message {
                role: MessageRole::Assistant,
                text: "new answer".into(),
            }),
        ];
        live.extend_events(events.clone());
        let frame = live.frame();
        assert_eq!(frame, agent_frame_from_events(&events));
        assert_eq!(frame.items.iter().filter(|item| matches!(item, AgentFrameItem::Message(message) if message.role == MessageRole::Assistant && message.text == "new answer")).count(), 1);
        assert!(!frame.items.iter().any(|item| matches!(item, AgentFrameItem::Message(message) if message.text.contains("failed prefix"))));
    }
}
