use std::time::Instant;

use crate::contract::*;

use super::types::{AgentFrame, AgentFrameItem};

/// Reducer-side turn bookkeeping threaded through [`apply_agent_event_to_frame`]
/// so an [`Event::TurnEnded`] fold can attach the turn's model id and
/// elapsed wall-clock duration to its `AgentFrameItem::TurnEnded` -- see
/// `docs/agent-output-ui-amendment.md`'s 2026-07-12 addendum.
///
/// Not stored on `AgentFrame` itself, for the same reason [`StateEntry`]
/// isn't: `AgentFrame` derives `Eq`/`PartialEq` and every caller (tests,
/// `live::State`, the UI's revision-memoized diffing) relies on comparing
/// frames deterministically -- an `Instant` field on the frame would make
/// that comparison time-sensitive. This is the sidecar instead.
///
/// Trade-off: `started_at` is an `Instant` captured at *fold* time, not a
/// timestamp carried on the wire `Event`. For a live fold (`live::State::
/// extend_provider_events`, called as events actually arrive) this measures
/// the turn's real wall-clock length. For a cold replay
/// (`agent_frame_from_events`, used for persisted-log bootstrap and
/// `duckdb`'s history queries) every historical event folds in one tight
/// loop, so the resulting `elapsed` collapses to however long the replay
/// itself took -- typically microseconds, not the turn's original duration.
/// No per-event timestamp is threaded through `Event` today to reconstruct
/// the original length exactly (`persistence::event_log::Record::
/// created_at_unix_ms` exists, but it's a *persistence* concern stamped by
/// `Appender` after the fact -- not visible to this crate's pure
/// `Event`-level fold). Accepted for stage A of the turn-receipts work
/// (`docs/tasks/backlog.md` item 16): a replayed old turn's receipt shows a
/// near-zero duration rather than an error or a missing field, and never
/// overstates elapsed. A precise persisted duration is a follow-up if it
/// turns out to matter -- deriving it via `duckdb`'s existing
/// `agent_events.created_at_unix_ms`, mirroring `agent_turns`'s own "no
/// derived durations, join through `ended_event_id`" choice, or threading a
/// timestamp onto `Event` itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TurnClock {
    started_at: Option<Instant>,
    model: Option<String>,
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
        Event::StateChanged(state) => frame.state = Some(*state),
        Event::ReasoningDelta(delta) => {
            if let Some(AgentFrameItem::ReasoningDelta(existing)) =
                last_current_turn_item_mut(frame, is_turn_boundary_item, |item| {
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
                last_current_turn_item_mut(frame, is_turn_boundary_item, |item| {
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
            // A fresh user message opens a new turn -- mirrors
            // `persistence::event_log::turn::TurnTracker`'s own opening
            // condition, so the reducer's notion of "current turn" for
            // elapsed-time purposes lines up with the persisted turn_id
            // grouping. Captured unconditionally (never gated on whether a
            // turn was already open): the session loop never sends a new
            // `UserMessage` until the previous turn settled
            // (`WaitingForUser`), so every occurrence really does start a
            // new turn. See `TurnClock`.
            // A background-`task` notification only opens a turn when none
            // is running -- the auto-turn wake case. Injected mid-turn it
            // belongs to the turn already in flight, whose elapsed clock
            // must keep running. Mirrors `TurnTracker`'s own condition, for
            // the same reason.
            let opens_turn = message.role == MessageRole::User
                || (message.role == MessageRole::TaskNotification && turn.started_at.is_none());
            if opens_turn {
                turn.started_at = Some(Instant::now());
                turn.model = None;
            }
            // A single provider response persists as streaming
            // `AssistantTextDelta`(s) emitted during the stream, plus this
            // one `MessageCommitted` carrying the full accumulated text,
            // emitted only once the stream ends -- after any
            // `ToolCallRequested`/`Started`/`Finished` events that arrived
            // mid-stream (see `completion::rig_openai_turn_streaming`). The
            // commit must promote the delta it corresponds to into a
            // `Message` at the delta's original position -- which may sit
            // *before* a tool call -- so the text renders once, before the
            // tool, rather than duplicating after it. The search crosses
            // tool-call/approval boundaries
            // (`is_turn_opening_boundary_item`, the loose boundary) to reach
            // a pre-tool delta.
            if promote_assistant_text_deltas_to_message(frame, message) {
                return;
            }
            // No streaming delta to promote: replace the last `Message` of
            // the same role in the current turn (strict boundary), or push a
            // new one.
            if let Some(index) =
                last_current_turn_item_index(frame, is_turn_boundary_item, |item| {
                    matches!(item, AgentFrameItem::Message(_))
                })
            {
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
            // Supersede a pending `ToolCallPreparing` progress item in
            // place, the same way `MessageCommitted` above replaces a
            // streaming `AssistantTextDelta` — otherwise the ephemeral
            // "preparing…" block would linger in the transcript right next
            // to the real tool-call block it was standing in for.
            if let Some(index) =
                last_current_turn_item_index(frame, is_turn_boundary_item, |item| {
                    matches!(item, AgentFrameItem::ToolCallPreparing(_))
                })
            {
                frame.items[index] = AgentFrameItem::ToolCallRequested(request.clone());
                return;
            }
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
        // Provider request lifecycle markers are timing-only (see their doc
        // comments on `Event`): they exist for persisted replay/inspection,
        // not for pane rendering, so they leave the frame untouched — the
        // same treatment `Event::StateChanged` gives `frame.state` without
        // an item, just with nothing to set. `ProviderRequestSent` is the
        // one exception: its `model` is remembered on `turn` (not pushed as
        // an item) so a later `TurnEnded` fold can attach it to the turn's
        // receipt.
        Event::ProviderRequestSent(sent) => {
            turn.model = Some(sent.model.clone());
        }
        Event::ProviderRequestFirstToken
        | Event::ProviderRequestFinished
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
        Event::MemoryDigest(digest) => {
            frame
                .items
                .push(AgentFrameItem::MemoryDigest(digest.clone()));
        }
        Event::MemoryCheckpointMissed => {
            frame.items.push(AgentFrameItem::MemoryCheckpointMissed);
        }
        Event::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        Event::Exited(exit) => frame.items.push(AgentFrameItem::Exited(exit.clone())),
        // Operator-intervention audit records (`Event::ApprovalResolved` /
        // `Event::ContinueTurnRequested`): deliberately fold into no frame
        // item. Their audit purpose is fully served by the raw record in
        // `agent_events` (which `LiveState::extend_provider_events` already
        // persists via the unconditional `self.events.push(event.event)`
        // below the `apply_agent_event_to_frame` call in
        // `live::State::extend_provider_events`), and the transcript already
        // shows their visible effect through the existing approval row /
        // `TurnEnded` receipt / next-turn events. Adding a frame item here
        // would duplicate that signal in the UI without adding anything
        // for SQL analytics that read `agent_events` directly.
        Event::ApprovalResolved(_) | Event::ContinueTurnRequested(_) => {}
        // The turn's receipt: see `Event::TurnEnded`'s doc comment and
        // `TurnClock`'s. `turn` is reset afterward so a stray second
        // `TurnEnded` with no intervening user message (shouldn't happen by
        // contract, but this keeps the reducer defensive) reports a
        // near-zero elapsed rather than reusing a stale start.
        Event::TurnEnded(reason) => {
            let elapsed = turn
                .started_at
                .map(|started_at| started_at.elapsed())
                .unwrap_or_default();
            frame.items.push(AgentFrameItem::TurnEnded {
                reason: *reason,
                model: turn.model.clone(),
                elapsed,
            });
            turn.started_at = None;
            turn.model = None;
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
        is_turn_boundary_item,
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
fn promote_assistant_text_deltas_to_message(frame: &mut AgentFrame, message: &Message) -> bool {
    let start = frame
        .items
        .iter()
        .rposition(is_turn_opening_boundary_item)
        .map_or(0, |index| index + 1);

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
