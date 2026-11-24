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
        Event::ReasoningDelta(delta) => {
            if let Some(AgentFrameItem::ReasoningDelta(existing)) =
                last_response_item_mut(frame, turn.response_start, |item| {
                    matches!(item, AgentFrameItem::ReasoningDelta(_))
                })
                && existing.role == delta.role
            {
                existing.text.push_str(&delta.text);
                return;
            }
            frame
                .items
                .push(AgentFrameItem::ReasoningDelta(delta.clone()));
        }
        Event::AssistantTextDelta(delta) => {
            if let Some(AgentFrameItem::AssistantTextDelta(existing)) =
                last_response_item_mut(frame, turn.response_start, |item| {
                    matches!(item, AgentFrameItem::AssistantTextDelta(_))
                })
                && existing.role == delta.role
            {
                existing.text.push_str(&delta.text);
                return;
            }
            frame
                .items
                .push(AgentFrameItem::AssistantTextDelta(delta.clone()));
        }
        Event::MessageCommitted(message) => {
            // A single provider response persists as streaming
            // `AssistantTextDelta`(s) emitted during the stream, plus this
            // one `MessageCommitted` carrying the full accumulated text,
            // emitted only once the stream ends -- after any
            // `ToolCallRequested`/`Started`/`Finished` events that arrived
            // mid-stream (see `completion::rig_provider_turn_streaming`). The
            // commit must promote the delta it corresponds to into a
            // `Message` at the delta's original position -- which may sit
            // *before* a tool call -- so the text renders once, before the
            // tool, rather than duplicating after it. The search crosses
            // tool-call/approval boundaries
            // (`is_turn_opening_boundary_item`, the loose boundary) to reach
            // a pre-tool delta.
            if promote_assistant_text_deltas_to_message(frame, message, turn.response_start) {
                return;
            }
            // No streaming delta to promote: replace the last `Message` of
            // the same role in the current turn (strict boundary), or push a
            // new one.
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
        // Request markers bound live previews without adding transcript rows.
        // Remember the model for the turn receipt as well.
        Event::ProviderRequestSent(_) => {
            // Failed attempts never commit their text. Retire only their live
            // preview; completed messages and diagnostic reasoning remain.
            let mut index = 0;
            frame.items.retain(|item| {
                let keep = index < turn.response_start
                    || !matches!(
                        item,
                        AgentFrameItem::AssistantTextDelta(_)
                            | AgentFrameItem::ToolCallPreparing(_)
                    );
                index += 1;
                keep
            });
            turn.response_start = frame.items.len();
        }
        Event::ProviderRequestFinished => {
            frame
                .items
                .retain(|item| !matches!(item, AgentFrameItem::ToolCallPreparing(_)));
            turn.response_start = turn.response_start.min(frame.items.len());
        }
        Event::ProviderRequestFirstToken | Event::ProviderRequestUsage(_) => {}
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

/// Folds one [`ToolCallProgress`] tick into the frame: updates the matching
/// in-flight `ToolCallPreparing` item in place (by `key`) if the current
/// turn already has one, otherwise starts a new one. Deliberately mirrors
/// the `ReasoningDelta`/`AssistantTextDelta` accumulation pattern in
/// [`apply_agent_event_to_frame`] — `ToolCallPreparing` is not a turn
/// boundary (see [`is_turn_boundary_item`]) for the same reason those
/// aren't: this needs to keep matching the same item across repeated calls
/// while it is the most recent thing in the turn.
pub(crate) fn apply_tool_call_progress_to_frame(
    frame: &mut AgentFrame,
    progress: ToolCallProgress,
) {
    if let Some(AgentFrameItem::ToolCallPreparing(existing)) = last_current_turn_item_mut(
        frame,
        is_turn_opening_boundary_item,
        |item| matches!(item, AgentFrameItem::ToolCallPreparing(existing) if existing.key == progress.key),
    ) {
        *existing = progress;
        return;
    }
    frame
        .items
        .push(AgentFrameItem::ToolCallPreparing(progress));
}

fn last_current_turn_item_mut(
    frame: &mut AgentFrame,
    boundary: impl Fn(&AgentFrameItem) -> bool,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<&mut AgentFrameItem> {
    let index = last_current_turn_item_index(frame, boundary, predicate)?;
    frame.items.get_mut(index)
}

/// Finds the last item matching `predicate` in the current turn, where the
/// turn's extent is defined by `boundary`: the search starts just after the
/// last item for which `boundary` returns true (or from the beginning if
/// none). The boundary is a parameter so different folds can scope their
/// lookups differently -- see [`is_turn_boundary_item`] (strict, used for
/// delta accumulation) and [`is_turn_opening_boundary_item`] (loose, used
/// for streaming-delta-to-message promotion).
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
/// delta/message/tool-call run for *accumulation* purposes. Used by
/// [`last_current_turn_item_mut`]/[`last_current_turn_item_index`] callers
/// that merge into the most recent item of a kind -- a `ToolCallRequested`
/// mid-stream must start a fresh accumulation window so post-tool streaming
/// text does not merge into the pre-tool delta.
///
/// Compare [`is_turn_opening_boundary_item`], the **loose** boundary used
/// only for streaming-delta-to-message promotion, which excludes
/// tool-call/approval items so a late `MessageCommitted` can reach back
/// across them to the pre-tool delta it corresponds to.
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

/// The **loose** turn boundary: only items that genuinely open or close a
/// turn (user/`task` messages, `TurnEnded`, `Error`, `Exited`), excluding
/// the tool-call and approval items that [`is_turn_boundary_item`] also
/// counts. Used solely by [`promote_assistant_text_deltas_to_message`]:
/// a provider emits a `MessageCommitted` (full text) *after* the
/// `ToolCallRequested`/`Started`/`Finished` events that arrived mid-stream,
/// so the commit's search for the streaming `AssistantTextDelta` it
/// promotes must reach across those tool-call boundaries to find the
/// pre-tool delta. See [`is_turn_boundary_item`]'s doc for the contrast.
fn is_turn_opening_boundary_item(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(Message {
            role: MessageRole::User | MessageRole::TaskNotification,
            ..
        }) | AgentFrameItem::TurnEnded { .. }
            | AgentFrameItem::Error(_)
            | AgentFrameItem::Exited(_)
    )
}

/// Promotes the streaming `AssistantTextDelta` items of `message.role` in
/// the current turn into a single committed [`AgentFrameItem::Message`],
/// placing it at the position of the first delta and removing any later
/// same-role deltas. Returns whether a promotion happened (i.e. at least
/// one delta was found).
///
/// A single provider response emits `AssistantTextDelta`(s) during the
/// stream and one `MessageCommitted` carrying the full text after the
/// stream ends -- after any tool-call events that arrived mid-stream. This
/// folds those back into one `Message` at the delta's original position
/// (before the tool call), so the text renders once, before the tool,
/// rather than duplicating after it. The search uses
/// [`is_turn_opening_boundary_item`] (the loose boundary) so it can reach a
/// pre-tool delta across intervening tool-call items.
///
/// For the rare "text → tool → text" response shape (text both before and
/// after a tool call within one stream), the first delta's position is used
/// and later same-role deltas are dropped -- their content is subsumed by
/// the full-text commit, and the text appears once, before the tool, per
/// the design intent that assistant text precedes its tool calls.
fn promote_assistant_text_deltas_to_message(
    frame: &mut AgentFrame,
    message: &Message,
    response_start: usize,
) -> bool {
    let start = frame
        .items
        .iter()
        .rposition(is_turn_opening_boundary_item)
        .map_or(0, |index| index + 1);

    let start = start.max(response_start).min(frame.items.len());
    let delta_indices: Vec<usize> = frame.items[start..]
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            AgentFrameItem::AssistantTextDelta(delta) if delta.role == message.role => {
                Some(start + i)
            }
            _ => None,
        })
        .collect();

    if delta_indices.is_empty() {
        return false;
    }

    // Place the committed Message at the first delta's position, then drop
    // any remaining same-role deltas (post-tool text in a "text → tool →
    // text" response) -- their content is already in the full text.
    let first = delta_indices[0];
    frame.items[first] = AgentFrameItem::Message(message.clone());
    for &index in delta_indices[1..].iter().rev() {
        frame.items.remove(index);
    }
    true
}

fn last_response_item_mut(
    frame: &mut AgentFrame,
    start: usize,
    predicate: impl Fn(&AgentFrameItem) -> bool,
) -> Option<&mut AgentFrameItem> {
    let index = last_current_turn_item_index(frame, is_turn_boundary_item, predicate)?;
    (index >= start).then(|| &mut frame.items[index])
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
        assert!(
            !frame
                .items
                .iter()
                .any(|item| matches!(item, AgentFrameItem::AssistantTextDelta(_)))
        );
    }
    #[test]
    fn finalizing_one_interleaved_preview_does_not_remove_its_sibling() {
        let live = crate::live::LiveState::with_disabled_persistence();
        for key in ["a", "b", "a"] {
            live.extend_provider_events([ProviderEvent::ToolCallProgress(ToolCallProgress {
                key: key.into(),
                tool_id: Some("fs_read".into()),
                bytes: 3,
            })])
            .unwrap();
        }
        live.extend_provider_events([ProviderEvent::ToolCallProgressClosed("a".into())])
            .unwrap();
        std::assert_matches!(
            &live.frame().items[..], [AgentFrameItem::ToolCallPreparing(progress)] if progress.key == "b"
        );
        live.extend_events([Event::ProviderRequestFinished]);
        assert!(live.frame().items.is_empty());
    }
}
