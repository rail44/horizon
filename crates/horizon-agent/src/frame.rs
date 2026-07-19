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
pub(crate) struct TurnClock {
    started_at: Option<Instant>,
    model: Option<String>,
}

impl TurnClock {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Tracks how long an [`AgentFrame`]'s `state` has held its current value,
/// for pane headers that show elapsed time in the current state (see
/// `docs/ux-principles.md`'s Persistent UI Requirement to show pane state).
///
/// `AgentFrame` itself doesn't carry this: its two-field shape is relied on
/// by callers that construct it as a plain struct literal, so timestamping
/// would live in this sidecar instead — a caller that needs it per session
/// would keep one alongside the frame and call [`Self::advance`] every time
/// it observes a new frame.
///
/// `cfg(test)`: no in-crate caller currently constructs one outside this
/// crate's own tests (confirmed by grep at the time of the 2026-07-18
/// interface audit) -- previously exempt from the dead-code lint only
/// because the type was `pub`.
#[derive(Clone, Copy, Debug)]
#[cfg(test)]
pub(crate) struct StateEntry {
    pub state: Option<SessionState>,
    entered_at: Instant,
}

#[cfg(test)]
impl StateEntry {
    pub(crate) fn initial(state: Option<SessionState>) -> Self {
        Self {
            state,
            entered_at: Instant::now(),
        }
    }

    /// Returns the entry that should be current after observing `state`:
    /// unchanged (same `entered_at`) if `state` matches, otherwise a fresh
    /// entry timestamped now.
    pub(crate) fn advance(self, state: Option<SessionState>) -> Self {
        if self.state == state {
            self
        } else {
            Self::initial(state)
        }
    }

    pub(crate) fn entered_at(&self) -> Instant {
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
    /// `ToolCallStarted` or `ToolCallFinished` resolves it -- see
    /// [`pending_approval_call_ids_in`]'s doc comment for the ack
    /// semantics). Backs [`Self::pending_approval_call_id`] below. The
    /// current approval UI reads the *actionable* queue instead
    /// ([`Self::actionable_pending_approval_call_ids`], via
    /// `AgentSession::pending_approval_call_ids` in `src/agent/session.rs`)
    /// for its row-centric rendering (owner decision 2026-07-13,
    /// superseding the old composer banner) -- see the tool-approval
    /// interaction rework's "targeting discipline" design note. Delegates
    /// to [`pending_approval_call_ids_in`] so a caller holding only the
    /// `items` field (not a whole `AgentFrame`) can reuse the exact same
    /// logic without a whole-frame clone.
    pub fn pending_approval_call_ids(&self) -> Vec<ToolCallId> {
        pending_approval_call_ids_in(&self.items)
    }

    /// The next tool-call approval to act on: the oldest still-pending
    /// request, if any. See [`Self::pending_approval_call_ids`].
    pub fn pending_approval_call_id(&self) -> Option<ToolCallId> {
        self.pending_approval_call_ids().into_iter().next()
    }

    /// [`Self::pending_approval_call_ids`], excluding any request whose
    /// own turn has since ended -- see
    /// [`actionable_pending_approval_call_ids_in`]'s doc comment. This is
    /// the version any *dispatch* path (the approve-tool-call/
    /// deny-tool-call palette commands, the command-availability gate
    /// deciding whether to offer them at all) must use, never the plain
    /// [`Self::pending_approval_call_ids`].
    pub fn actionable_pending_approval_call_ids(&self) -> Vec<ToolCallId> {
        actionable_pending_approval_call_ids_in(&self.items)
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

    /// The most recent `ApprovalRequested` item's [`ApprovalKind`] for
    /// `call_id`, if any -- what `tools::approval::resolve_bash` needs to
    /// tell a domain-denial retry apart from an ordinary approval or a
    /// sandbox-denial retry (`docs/agent-approval-design.md` leg 4b), the
    /// same way [`Self::tool_call_request`] recovers a pending call's
    /// `tool_id`/`input`.
    pub fn approval_kind(&self, call_id: &ToolCallId) -> Option<ApprovalKind> {
        self.items.iter().rev().find_map(|item| match item {
            AgentFrameItem::ApprovalRequested(request) if &request.call_id == call_id => {
                Some(request.kind.clone())
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

    /// Whether `call_id`'s *most recent* `ToolCallRequested` occurrence
    /// already has a terminal `ToolCallFinished` in the frame — from an
    /// earlier approve/deny short-circuit, a genuine result, or a
    /// cancellation that finished the call. Used to guard against
    /// double-folding a late result that arrives after the call already
    /// resolved: `agent::tools::approval`'s `ApprovalOutcome::AlreadyResolved`
    /// check, and the bash tool's `should_fold_completion` (called from
    /// `horizon-sessiond`'s `fold_bash_completion`), both key off this.
    ///
    /// Scoped to items at-or-after the latest `ToolCallRequested` for
    /// `call_id` (same "most recent occurrence" reading
    /// [`Self::tool_call_request`] already uses), not the whole session —
    /// root-caused 2026-07-18: a provider can reuse the exact same
    /// call_id string for a second, distinct call after an earlier
    /// occurrence's full request/approve/finish cycle already closed (a
    /// real rig/OpenAI-compatible-backend quirk, not just theoretical).
    /// Scanning the whole session here mistook that stale earlier finish
    /// for the current occurrence's, permanently short-circuiting every
    /// approve/deny for the new call as `AlreadyResolved` and wedging the
    /// turn — the daemon-side half of the "session stuck on an edit call
    /// with no working Approve" report; [`Self::tool_call_request`]'s own
    /// `.rev()` scoping was already immune to this.
    pub fn has_tool_call_finished(&self, call_id: &ToolCallId) -> bool {
        self.items_since_latest_request(call_id).any(|item| {
            matches!(item, AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id)
        })
    }

    /// Whether `call_id`'s *most recent* `ToolCallRequested` occurrence
    /// already has a `ToolCallStarted` in the frame — true from the
    /// instant Horizon began executing it, not just once it finished. For
    /// `fs.write`/`fs.edit` this is equivalent to
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
    /// same still-running bash call). Same reused-call_id scoping as
    /// [`Self::has_tool_call_finished`] — see its doc comment.
    pub fn has_tool_call_started(&self, call_id: &ToolCallId) -> bool {
        self.items_since_latest_request(call_id)
            .any(|item| matches!(item, AgentFrameItem::ToolCallStarted(id) if id == call_id))
    }

    /// The frame's items from (and including) `call_id`'s most recent
    /// `ToolCallRequested` onward, or the whole slice if `call_id` was
    /// never requested at all (harmless: every caller only matches items
    /// that reference `call_id`, so an unrelated call_id still yields
    /// `false`/`None` either way). Shared scoping helper for
    /// [`Self::has_tool_call_finished`]/[`Self::has_tool_call_started`].
    fn items_since_latest_request(
        &self,
        call_id: &ToolCallId,
    ) -> impl Iterator<Item = &AgentFrameItem> {
        let start = self
            .items
            .iter()
            .rposition(|item| {
                matches!(item, AgentFrameItem::ToolCallRequested(request) if &request.call_id == call_id)
            })
            .unwrap_or(0);
        self.items[start..].iter()
    }
}

/// The pure core of [`AgentFrame::pending_approval_call_ids`], operating on
/// just an `items` slice rather than a whole `AgentFrame` -- lets a caller
/// holding only a slice of items, not a whole frame, reuse this logic
/// directly. `crate::transcript::tool_call::is_approval_still_pending` is
/// exactly this kind of caller: it only has a single turn's own item
/// slice, not the whole session frame, when it asks whether a call is
/// still pending.
///
/// Ack semantics (root-caused 2026-07-13 -- the daemon's approve/deny round
/// trip does not wait for the tool to finish): `crate::tools::approval::
/// resolve_approval` folds a decision's *first* synchronous ack one IPC hop
/// after the click -- `ToolCallStarted` for an approve (execution has begun;
/// for `bash` that's the *only* immediate ack, since the result arrives
/// later and asynchronously -- see `AgentFrame::has_tool_call_started`'s doc
/// comment), or `ToolCallFinished` directly for a deny (short-circuited,
/// nothing ever starts) or for a synchronous tool whose approve folds
/// `ToolCallStarted` and `ToolCallFinished` together in the same round trip.
/// Either ack resolves the pending entry here: the user's decision has
/// already been acted on, so there is nothing left pending a UI reaction to
/// -- only the tool's eventual *result* (irrelevant to this queue) is still
/// outstanding for `bash`.
pub fn pending_approval_call_ids_in(items: &[AgentFrameItem]) -> Vec<ToolCallId> {
    let mut pending = Vec::<ToolCallId>::new();
    for item in items {
        match item {
            AgentFrameItem::ApprovalRequested(request) if !pending.contains(&request.call_id) => {
                pending.push(request.call_id.clone());
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                pending.retain(|pending_id| pending_id != call_id);
            }
            AgentFrameItem::ToolCallFinished(result) => {
                pending.retain(|call_id| call_id != &result.call_id);
            }
            _ => {}
        }
    }

    pending
}

/// [`pending_approval_call_ids_in`], with one more rule: a `TurnEnded`
/// clears every request still outstanding at that point, since none of
/// them can ever resolve normally now (root-caused 2026-07-13 -- see
/// `docs/agent-output-ui-amendment.md`'s post-review note). An
/// `ApprovalRequested` whose own turn ended without a matching
/// `ToolCallFinished` is a *ghost*: mid-turn interjection (sending
/// another message while an earlier tool call's approval is still
/// unresolved -- next-turn delivery is deliberate even mid-flight,
/// decision 6) can leave one behind, and the session loop that owned its
/// approval gate has since moved on to a different turn entirely, so
/// there is no live daemon-side gate left to answer a decision for it.
///
/// [`pending_approval_call_ids_in`] itself is deliberately left alone --
/// `turns::is_approval_still_pending` (the completed-turn transcript's
/// defensive "still shows a dangling approval box" case) needs the
/// *unscoped* reading precisely because it's asking about a request
/// within its own already-ended turn's item slice, where the turn's own
/// closing `TurnEnded` is the last item; scoping would make that check
/// always report "not pending" and silently swallow the one case it
/// exists to catch. This function is for every other caller: anything
/// that dispatches an approve/deny by picking the oldest pending
/// call_id (the palette's approve-tool-call/deny-tool-call commands) or
/// decides whether to offer them at all -- those must never target or
/// advertise a ghost, since doing so blocks every later dispatch behind
/// a call that can no longer resolve (the "one approval worked, then
/// everything looked permanently stuck" report).
pub fn actionable_pending_approval_call_ids_in(items: &[AgentFrameItem]) -> Vec<ToolCallId> {
    let mut pending = Vec::<ToolCallId>::new();
    for item in items {
        match item {
            AgentFrameItem::ApprovalRequested(request) if !pending.contains(&request.call_id) => {
                pending.push(request.call_id.clone());
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                pending.retain(|pending_id| pending_id != call_id);
            }
            AgentFrameItem::ToolCallFinished(result) => {
                pending.retain(|call_id| call_id != &result.call_id);
            }
            AgentFrameItem::TurnEnded { .. } => pending.clear(),
            _ => {}
        }
    }

    pending
}

/// Whether the frame's last item is a guard-halted `TurnEnded` -- i.e. the
/// session is sitting on a paused turn `Command::ContinueTurn` can resume
/// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution,
/// decision 3). Only the *last* item counts: any later activity (a new user
/// message, a fresh tool call) means the halt has already been superseded,
/// even though the old `TurnEnded { reason: Halted*, .. }` item is still
/// sitting earlier in the frame. `TurnEndReason::Halted` (the legacy,
/// pre-resolution bare variant -- see its own doc comment) counts too: an
/// old persisted session that halted before this resolution still reads as
/// paused and offers Continue, it just can't say which guard fired.
pub fn halted_awaiting_continue(items: &[AgentFrameItem]) -> bool {
    matches!(
        items.last(),
        Some(AgentFrameItem::TurnEnded {
            reason: TurnEndReason::Halted
                | TurnEndReason::HaltedByIterationCap
                | TurnEndReason::HaltedByDoomLoop,
            ..
        })
    )
}

/// The pure core of [`AgentFrame::is_turn_in_flight`], operating on just the
/// `state` field -- see [`pending_approval_call_ids_in`]'s doc comment for
/// why this split exists. `src/agent/view.rs`'s render and
/// `sync_running_turn_clock` are exactly this kind of caller: each reads
/// `frame.state` on its own, separately from `frame.items`, without going
/// through [`AgentFrame::is_turn_in_flight`].
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

pub(crate) fn apply_agent_event_to_frame(
    frame: &mut AgentFrame,
    event: &Event,
    turn: &mut TurnClock,
) {
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
pub(crate) fn apply_tool_call_progress_to_frame(
    frame: &mut AgentFrame,
    progress: ToolCallProgress,
) {
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
    //! `actionable_pending_approval_call_ids_in` (2026-07-13) is the
    //! ghost-excluding sibling of `pending_approval_call_ids_in` -- see
    //! its own doc comment for why the two must stay distinct.
    use super::*;

    fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(call_id.to_string()),
            reason: "writes a file".to_string(),
            kind: ApprovalKind::Standard,
        })
    }

    fn tool_call_finished(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            serde_json::json!({}),
        ))
    }

    fn tool_call_finished_denied(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            serde_json::json!({ "is_error": true, "message": "denied by user" }),
        ))
    }

    fn tool_call_started(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallStarted(ToolCallId(call_id.to_string()))
    }

    fn turn_ended() -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason: TurnEndReason::Completed,
            model: None,
            elapsed: Duration::from_secs(1),
        }
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_excludes_a_ghost_ordered_before_a_live_request() {
        // Root-caused 2026-07-13: a mid-turn interjection can leave an
        // earlier approval unresolved when its own turn ends -- that
        // ghost must never sit ahead of the genuinely live one in
        // dispatch order.
        let items = vec![
            approval_requested("ghost"),
            turn_ended(),
            approval_requested("live"),
        ];
        assert_eq!(
            actionable_pending_approval_call_ids_in(&items),
            vec![ToolCallId("live".to_string())]
        );
        // The unscoped reading, by contrast, still (correctly, for its
        // own different purpose) reports both -- `is_approval_still_
        // pending`'s defensive completed-turn check relies on exactly
        // this to notice a request that never resolved before its own
        // turn ended.
        assert_eq!(
            pending_approval_call_ids_in(&items),
            vec![
                ToolCallId("ghost".to_string()),
                ToolCallId("live".to_string())
            ]
        );
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_is_empty_once_every_pending_turn_has_ended() {
        let items = vec![
            approval_requested("a"),
            approval_requested("b"),
            turn_ended(),
        ];
        assert_eq!(actionable_pending_approval_call_ids_in(&items), Vec::new());
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_matches_the_plain_reading_within_one_open_turn() {
        // No `TurnEnded` in sight yet: both readings agree, so a normal,
        // still-in-flight approval dispatches exactly as before.
        let items = vec![approval_requested("a"), approval_requested("b")];
        assert_eq!(
            actionable_pending_approval_call_ids_in(&items),
            pending_approval_call_ids_in(&items)
        );
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_still_resolves_normally_within_its_own_turn() {
        // A request resolved (approved/denied) before its own turn ends
        // is not a ghost -- it should never appear at all, scoped or not.
        let items = vec![
            approval_requested("a"),
            tool_call_finished("a"),
            turn_ended(),
            approval_requested("b"),
        ];
        assert_eq!(
            actionable_pending_approval_call_ids_in(&items),
            vec![ToolCallId("b".to_string())]
        );
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
    fn pending_approval_call_ids_in_resolves_on_tool_call_started() {
        // The daemon's approve ack for `bash` folds `ToolCallStarted`
        // synchronously, one IPC hop after the click, with the eventual
        // `ToolCallFinished` arriving later and asynchronously -- see
        // `resolve_bash`/`ApprovalOutcome::Started`
        // (`crates/horizon-agent/src/tools/approval.rs`). The pending queue
        // must resolve right there, not wait for the result.
        let items = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_call_started("a"),
        ];
        assert_eq!(
            pending_approval_call_ids_in(&items),
            vec![ToolCallId("b".to_string())]
        );

        // A later `ToolCallFinished` for the same call is a no-op on this
        // queue -- it already left the moment `ToolCallStarted` folded.
        let items = vec![
            approval_requested("a"),
            tool_call_started("a"),
            tool_call_finished("a"),
        ];
        assert_eq!(pending_approval_call_ids_in(&items), Vec::new());
    }

    #[test]
    fn pending_approval_call_ids_in_resolves_a_denied_finish() {
        // A deny never folds `ToolCallStarted` (nothing ever executes) --
        // its own `ToolCallFinished`, carrying the "denied by user"
        // convention, is the ack that resolves it.
        let items = vec![approval_requested("a"), tool_call_finished_denied("a")];
        assert_eq!(pending_approval_call_ids_in(&items), Vec::new());
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_resolves_on_tool_call_started() {
        let items = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_call_started("a"),
        ];
        assert_eq!(
            actionable_pending_approval_call_ids_in(&items),
            vec![ToolCallId("b".to_string())]
        );
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_resolves_a_denied_finish() {
        let items = vec![approval_requested("a"), tool_call_finished_denied("a")];
        assert_eq!(actionable_pending_approval_call_ids_in(&items), Vec::new());
    }

    #[test]
    fn actionable_pending_approval_call_ids_in_still_excludes_a_ghost_once_a_later_request_has_started(
    ) {
        // The realistic post-fix shape of the ghost repro: an earlier
        // approval never got decided before its own turn ended (a ghost),
        // and a later turn's request was since approved -- its
        // `ToolCallStarted` ack folded, but the tool hasn't finished yet.
        // Nothing is left actionable: the ghost can never resolve and the
        // live one already has its decision.
        let items = vec![
            approval_requested("ghost"),
            turn_ended(),
            approval_requested("live"),
            tool_call_started("live"),
        ];
        assert_eq!(actionable_pending_approval_call_ids_in(&items), Vec::new());
        // The unscoped reading still reports the ghost -- unaffected by
        // the `ToolCallStarted` ack rule, same as before this change.
        assert_eq!(
            pending_approval_call_ids_in(&items),
            vec![ToolCallId("ghost".to_string())]
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

    fn turn_ended_with_reason(reason: TurnEndReason) -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason,
            model: None,
            elapsed: Duration::from_secs(1),
        }
    }

    #[test]
    fn halted_awaiting_continue_is_true_for_either_specific_guard_reason() {
        assert!(halted_awaiting_continue(&[turn_ended_with_reason(
            TurnEndReason::HaltedByIterationCap
        )]));
        assert!(halted_awaiting_continue(&[turn_ended_with_reason(
            TurnEndReason::HaltedByDoomLoop
        )]));
    }

    #[test]
    fn halted_awaiting_continue_is_true_for_the_legacy_bare_halted_reason() {
        // A pre-resolution persisted session used the bare `Halted` variant
        // -- it must still read as a resumable pause, just without a
        // specific guard-kind sentence available.
        assert!(halted_awaiting_continue(&[turn_ended_with_reason(
            TurnEndReason::Halted
        )]));
    }

    #[test]
    fn halted_awaiting_continue_is_false_for_a_normal_end_reason() {
        assert!(!halted_awaiting_continue(&[turn_ended()]));
        assert!(!halted_awaiting_continue(&[turn_ended_with_reason(
            TurnEndReason::Cancelled
        )]));
        assert!(!halted_awaiting_continue(&[turn_ended_with_reason(
            TurnEndReason::Failed
        )]));
    }

    #[test]
    fn halted_awaiting_continue_is_false_once_superseded_by_later_activity() {
        // A halt sitting earlier in the frame no longer counts once a new
        // user message (or any other later item) has superseded it.
        let items = vec![
            turn_ended_with_reason(TurnEndReason::HaltedByIterationCap),
            AgentFrameItem::Message(Message {
                role: MessageRole::User,
                text: "hello again".to_string(),
            }),
        ];
        assert!(!halted_awaiting_continue(&items));
    }

    #[test]
    fn halted_awaiting_continue_is_false_for_an_empty_frame() {
        assert!(!halted_awaiting_continue(&[]));
    }
}
