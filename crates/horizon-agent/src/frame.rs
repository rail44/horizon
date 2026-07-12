use std::time::{Duration, Instant};

use crate::contract::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentFrame {
    pub state: Option<SessionState>,
    pub items: Vec<AgentFrameItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentFrameItem {
    Message(Message),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallId),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    /// Ephemeral tool-call-argument-streaming progress (see
    /// [`ToolCallProgress`]): folded in place by
    /// [`apply_tool_call_progress_to_frame`] while arguments stream, and
    /// superseded in place once the real `ToolCallRequested` arrives (see
    /// the `Event::ToolCallRequested` arm in
    /// [`apply_agent_event_to_frame`]). Never produced by
    /// `agent_frame_from_events`/persisted replay — it never reaches the
    /// event log in the first place (`ProviderEvent::tool_call_progress`).
    ToolCallPreparing(ToolCallProgress),
    Error(Error),
    Exited(Exit),
    /// A turn's receipt: the end reason `Event::TurnEnded` carries, plus the
    /// model id and elapsed duration folded in at reducer time -- see
    /// `docs/agent-output-ui-amendment.md`'s 2026-07-12 addendum (decision
    /// 1's turn-receipt line, decision 2's running-card footer) and
    /// [`TurnClock`]'s doc comment for the elapsed-time trade-off.
    TurnEnded {
        reason: TurnEndReason,
        /// The model id reported by the turn's most recent
        /// `Event::ProviderRequestSent`, if the turn made any provider
        /// request at all (a turn that ends before one -- e.g. an
        /// immediate cancel -- has none).
        model: Option<String>,
        /// Wall-clock time from the turn's opening `MessageCommitted`
        /// (`MessageRole::User`) to this fold. See [`TurnClock`].
        elapsed: Duration,
    },
}

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
pub struct TurnClock {
    started_at: Option<Instant>,
    model: Option<String>,
}

impl TurnClock {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Tracks how long an [`AgentFrame`]'s `state` has held its current value,
/// for pane headers that show elapsed time in the current state (see
/// `docs/ux-principles.md`'s Persistent UI Requirement to show pane state).
///
/// `AgentFrame` itself doesn't carry this: its two-field shape is relied on
/// by callers that construct it as a plain struct literal, so timestamping
/// lives in this sidecar instead — callers that need it per session (see
/// `session::Frames`) keep one alongside the frame and call [`Self::advance`]
/// every time they observe a new frame.
#[derive(Clone, Copy, Debug)]
pub struct StateEntry {
    pub state: Option<SessionState>,
    entered_at: Instant,
}

impl StateEntry {
    pub fn initial(state: Option<SessionState>) -> Self {
        Self {
            state,
            entered_at: Instant::now(),
        }
    }

    /// Returns the entry that should be current after observing `state`:
    /// unchanged (same `entered_at`) if `state` matches, otherwise a fresh
    /// entry timestamped now.
    pub fn advance(self, state: Option<SessionState>) -> Self {
        if self.state == state {
            self
        } else {
            Self::initial(state)
        }
    }

    pub fn entered_at(&self) -> Instant {
        self.entered_at
    }
}

impl AgentFrame {
    pub fn empty() -> Self {
        Self {
            state: None,
            items: Vec::new(),
        }
    }

    /// All currently-pending tool-call approvals, oldest request first (an
    /// `ApprovalRequested` counts as pending until a matching
    /// `ToolCallFinished` resolves it). Backs both `pending_approval_call_id`
    /// below and the approval banner's queue depth
    /// (`workspace::view::agent_controls::agent_approval_banner`), which
    /// answers the oldest pending call first and shows a "+N more" hint for
    /// the rest -- see the tool-approval interaction rework's "targeting
    /// discipline" design note. Delegates to [`pending_approval_call_ids_in`]
    /// so a caller holding only the `items` field (not a whole `AgentFrame`)
    /// -- e.g. `session::frames::AgentFrameHandle`'s own `items` signal --
    /// can reuse the exact same logic without a whole-frame clone.
    pub fn pending_approval_call_ids(&self) -> Vec<ToolCallId> {
        pending_approval_call_ids_in(&self.items)
    }

    /// The next tool-call approval to act on: the oldest still-pending
    /// request, if any. See [`Self::pending_approval_call_ids`].
    pub fn pending_approval_call_id(&self) -> Option<ToolCallId> {
        self.pending_approval_call_ids().into_iter().next()
    }

    /// The most recent `ToolCallRequested` item for `call_id`, if any. Used
    /// to recover a pending tool call's `tool_id`/`input` at approval time,
    /// since the approve/deny UI only carries the `call_id` forward.
    pub fn tool_call_request(&self, call_id: &ToolCallId) -> Option<&ToolCallRequest> {
        self.items.iter().rev().find_map(|item| match item {
            AgentFrameItem::ToolCallRequested(request) if &request.call_id == call_id => {
                Some(request)
            }
            _ => None,
        })
    }

    /// Whether a turn is currently in flight (streaming, running a tool, or
    /// waiting on tool-call approval) and therefore cancellable. Delegates
    /// to [`state_indicates_turn_in_flight`] -- see that function's doc
    /// comment for why this is factored out.
    pub fn is_turn_in_flight(&self) -> bool {
        state_indicates_turn_in_flight(self.state)
    }

    /// Whether `call_id` already has a terminal `ToolCallFinished` in the
    /// frame — from an earlier approve/deny short-circuit, a genuine result,
    /// or a cancellation that finished the call. Used to guard against
    /// double-folding a late result that arrives after the call already
    /// resolved: `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved`
    /// check, and the bash tool's async completion delivery
    /// (`app/runtime/agent.rs`), both key off this.
    pub fn has_tool_call_finished(&self, call_id: &ToolCallId) -> bool {
        self.items.iter().any(|item| {
            matches!(item, AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id)
        })
    }

    /// Whether `call_id` already has a `ToolCallStarted` in the frame —
    /// true from the instant Horizon began executing it, not just once it
    /// finished. For `fs.write`/`fs.edit` this is equivalent to
    /// [`Self::has_tool_call_finished`] (both events are folded together,
    /// synchronously, by `agent::tools::approval::synchronous_result`), but
    /// `bash`'s approve path folds `ToolCallStarted` immediately and only
    /// folds `ToolCallFinished` once the child actually exits — a window
    /// that can last the tool's whole timeout. `agent::tools::approval::
    /// try_execute`'s idempotence guard checks this *in addition to*
    /// `has_tool_call_finished` so a call already running can't be started a
    /// second time by a duplicate Approve arriving in that window (the
    /// 2026-07 repeated-approval OOM incident: a banner that didn't
    /// visibly react to a held-down `y` key each re-sent `Approve` for the
    /// same still-running bash call).
    pub fn has_tool_call_started(&self, call_id: &ToolCallId) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::ToolCallStarted(id) if id == call_id))
    }
}

/// The pure core of [`AgentFrame::pending_approval_call_ids`], operating on
/// just an `items` slice rather than a whole `AgentFrame` -- lets a caller
/// holding only the `items` field signal (not `state` too) reuse this logic
/// without constructing a whole frame first. `session::frames::
/// AgentFrameHandle` (`docs/reactive-store-design.md`'s foundation-5 Frames
/// migration) is exactly this kind of caller: reading only `items` there
/// means a pane's pending-approval indicator doesn't also subscribe to
/// `state` changes it has no use for.
pub fn pending_approval_call_ids_in(items: &[AgentFrameItem]) -> Vec<ToolCallId> {
    let mut pending = Vec::<ToolCallId>::new();
    for item in items {
        match item {
            AgentFrameItem::ApprovalRequested(request) if !pending.contains(&request.call_id) => {
                pending.push(request.call_id.clone());
            }
            AgentFrameItem::ToolCallFinished(result) => {
                pending.retain(|call_id| call_id != &result.call_id);
            }
            _ => {}
        }
    }

    pending
}

/// The pure core of [`AgentFrame::is_turn_in_flight`], operating on just the
/// `state` field -- see [`pending_approval_call_ids_in`]'s doc comment for
/// why this split exists: `AgentFrameHandle::state`'s own field signal can
/// answer "is a turn in flight" without a caller reading `items` too.
pub fn state_indicates_turn_in_flight(state: Option<SessionState>) -> bool {
    matches!(
        state,
        Some(SessionState::Running | SessionState::WaitingForApproval | SessionState::ToolRunning)
    )
}

#[cfg(test)]
pub fn render_agent_transcript(events: &[Event]) -> String {
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
            Event::ProviderRequestSent(sent) => {
                lines.push(format!("provider request sent: {}", sent.model));
            }
            Event::ProviderRequestFirstToken => {
                lines.push("provider request first token".to_string());
            }
            Event::ProviderRequestFinished => {
                lines.push("provider request finished".to_string());
            }
            Event::Error(error) => lines.push(format!("error: {}", error.message)),
            Event::Exited(exit) => lines.push(format!("exited: {}", exit.reason)),
            Event::TurnEnded(reason) => lines.push(format!("turn ended: {reason:?}")),
        }
    }

    lines.join("\n")
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

pub fn apply_agent_event_to_frame(frame: &mut AgentFrame, event: &Event, turn: &mut TurnClock) {
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
            // A fresh user message opens a new turn -- mirrors
            // `persistence::event_log::turn::TurnTracker`'s own opening
            // condition, so the reducer's notion of "current turn" for
            // elapsed-time purposes lines up with the persisted turn_id
            // grouping. Captured unconditionally (never gated on whether a
            // turn was already open): the session loop never sends a new
            // `UserMessage` until the previous turn settled
            // (`WaitingForUser`), so every occurrence really does start a
            // new turn. See `TurnClock`.
            if message.role == MessageRole::User {
                turn.started_at = Some(Instant::now());
                turn.model = None;
            }
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
            // Supersede a pending `ToolCallPreparing` progress item in
            // place, the same way `MessageCommitted` above replaces a
            // streaming `AssistantTextDelta` — otherwise the ephemeral
            // "preparing…" block would linger in the transcript right next
            // to the real tool-call block it was standing in for.
            if let Some(index) = last_current_turn_item_index(frame, |item| {
                matches!(item, AgentFrameItem::ToolCallPreparing(_))
            }) {
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
        Event::ProviderRequestFirstToken | Event::ProviderRequestFinished => {}
        Event::Error(error) => frame.items.push(AgentFrameItem::Error(error.clone())),
        Event::Exited(exit) => frame.items.push(AgentFrameItem::Exited(exit.clone())),
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
pub fn apply_tool_call_progress_to_frame(frame: &mut AgentFrame, progress: ToolCallProgress) {
    if let Some(AgentFrameItem::ToolCallPreparing(existing)) = last_current_turn_item_mut(
        frame,
        |item| matches!(item, AgentFrameItem::ToolCallPreparing(existing) if existing.key == progress.key),
    ) {
        *existing = progress;
        return;
    }
    frame
        .items
        .push(AgentFrameItem::ToolCallPreparing(progress));
}

/// The complete set of item indices a *next* in-place fold
/// (no push, `frame.items.len()` unchanged) could target -- the single
/// source of truth `diff_block_content`
/// (`src/agent/view/transcript.rs`) uses to know which blocks might have
/// changed on a frame update that didn't append a new item, instead of
/// assuming the literal last item.
///
/// Must stay in lockstep with [`apply_agent_event_to_frame`]'s in-place
/// arms, and [`apply_tool_call_progress_to_frame`]:
/// - `Event::ReasoningDelta` coalesces into the last `ReasoningDelta`.
/// - `Event::AssistantTextDelta` coalesces into the last
///   `AssistantTextDelta`.
/// - `Event::MessageCommitted` replaces the last `AssistantTextDelta`
///   (role match) or otherwise the last `Message` (role match).
/// - `Event::ToolCallRequested` supersedes the last `ToolCallPreparing`.
/// - `apply_tool_call_progress_to_frame`'s progress ticks update the last
///   matching `ToolCallPreparing` in place.
///
/// The first four of those are scoped to the current turn segment (from the
/// last [`is_turn_boundary_item`] to the end of `frame.items`) via
/// [`last_current_turn_item_index`] -- the same reverse scan the reducer
/// itself uses, reused rather than duplicated. That segment scoping is why
/// this can reach further back than the literal last item: within one turn,
/// a `ReasoningDelta` and an `AssistantTextDelta` can each hold their own
/// coalescing target at different indices (interleaved-thinking providers
/// alternate reasoning and text within a turn).
///
/// The literal last item is *also* always included, unconditionally: a
/// `ToolCallRequested` superseding a `ToolCallPreparing` changes that slot's
/// item *variant*, and `ToolCallRequested` is itself a turn boundary
/// (`is_turn_boundary_item`) -- so once superseded, a segment-scoped search
/// for `ToolCallPreparing` on the post-mutation frame always excludes that
/// very slot (it now starts, rather than sits inside, the next segment). No
/// type-scoped backward scan over the post-mutation frame can recover that
/// index; only "the literal last item" reliably can, since supersession
/// only ever happens at the current turn's most recent slot.
///
/// Adding a new in-place-mutation arm to the reducer means adding its
/// target kind here too.
///
/// Known limitation: the `ToolCallPreparing` target is the *last* one per
/// turn segment only, same as the reducer's own
/// `apply_tool_call_progress_to_frame` (keyed by matching `key`, but still
/// only ever searching for "the last matching item"). Genuinely concurrent
/// multi-key tool-argument streaming (two different in-flight preparing
/// items in the same turn segment, each ticking independently) would leave
/// a non-last preparing item's byte count stale here. Not reachable today:
/// the rig provider streams one tool's arguments at a time into a single
/// shared progress buffer, and the reducer's `ToolCallRequested`
/// supersession arm is itself unkeyed (matches "the last `ToolCallPreparing`",
/// not "the one with this call's key"), so concurrent preparing isn't
/// cleanly supported by the reducer either -- fully-keyed handling on both
/// sides is deferred to the airtight "reducer reports the mutated index"
/// follow-up this function is a stopgap for.
pub fn in_place_mutable_item_indices(frame: &AgentFrame) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut push_index = |index: Option<usize>| {
        if let Some(index) = index {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
    };
    push_index(frame.items.len().checked_sub(1));
    let mut push_target = |predicate: fn(&AgentFrameItem) -> bool| {
        push_index(last_current_turn_item_index(frame, predicate));
    };
    push_target(|item| matches!(item, AgentFrameItem::ReasoningDelta(_)));
    push_target(|item| matches!(item, AgentFrameItem::AssistantTextDelta(_)));
    push_target(|item| matches!(item, AgentFrameItem::Message(_)));
    push_target(|item| matches!(item, AgentFrameItem::ToolCallPreparing(_)));
    indices
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
            | AgentFrameItem::TurnEnded { .. }
    )
}

#[cfg(test)]
fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

#[cfg(test)]
mod field_scoped_reads_tests {
    //! `pending_approval_call_ids_in`/`state_indicates_turn_in_flight` are
    //! the pure logic factored out of `AgentFrame::pending_approval_call_ids`/
    //! `is_turn_in_flight` (`docs/reactive-store-design.md`'s foundation-5
    //! agent-consumer migration) so a caller holding only one `AgentFrame`
    //! field's signal can compute the same answer without the other field.
    //! `AgentFrame`'s own methods already delegate to these, so this pins
    //! the extraction didn't change behavior.
    use super::*;

    fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(call_id.to_string()),
            reason: "writes a file".to_string(),
        })
    }

    fn tool_call_finished(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            serde_json::json!({}),
        ))
    }

    #[test]
    fn pending_approval_call_ids_in_tracks_requests_and_resolutions() {
        assert_eq!(pending_approval_call_ids_in(&[]), Vec::new());

        let items = vec![approval_requested("a"), approval_requested("b")];
        assert_eq!(
            pending_approval_call_ids_in(&items),
            vec![ToolCallId("a".to_string()), ToolCallId("b".to_string())]
        );

        let items = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_call_finished("a"),
        ];
        assert_eq!(
            pending_approval_call_ids_in(&items),
            vec![ToolCallId("b".to_string())]
        );
    }

    #[test]
    fn state_indicates_turn_in_flight_matches_the_in_flight_states_only() {
        assert!(state_indicates_turn_in_flight(Some(SessionState::Running)));
        assert!(state_indicates_turn_in_flight(Some(
            SessionState::WaitingForApproval
        )));
        assert!(state_indicates_turn_in_flight(Some(
            SessionState::ToolRunning
        )));

        assert!(!state_indicates_turn_in_flight(None));
        assert!(!state_indicates_turn_in_flight(Some(SessionState::Created)));
        assert!(!state_indicates_turn_in_flight(Some(
            SessionState::Completed
        )));
    }
}
