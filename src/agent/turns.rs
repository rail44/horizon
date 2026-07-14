//! Pure view-model for turn grouping and receipt summarization
//! (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2). Kept
//! separate from `view.rs` so the grouping/aggregation logic has
//! colocated tests independent of GPUI rendering, and out of
//! `horizon-agent` so that crate stays UI-agnostic (verb naming, chip
//! composition, and humanized durations are display concerns, not
//! contract ones).

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use horizon_agent::contract::{Message, MessageRole, ToolCallId, ToolCallResult, TurnEndReason};
use horizon_agent::frame::{pending_approval_call_ids_in, AgentFrameItem};
use serde_json::Value;

/// One turn's items, sliced from `AgentFrame::items` by index range
/// `[start, end)`. `ended` is `None` for the turn currently in
/// progress -- the last span produced by [`group_into_turns`], and only
/// meaningful to render as such while the session state indicates a turn
/// is in flight (`horizon_agent::frame::state_indicates_turn_in_flight`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnSpan {
    pub start: usize,
    pub end: usize,
    pub ended: Option<TurnEnd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnEnd {
    pub reason: TurnEndReason,
    pub model: Option<String>,
    pub elapsed: Duration,
}

/// Groups `items` into turn segments: a segment opens at the first item
/// seen while no segment is currently open (whatever its type -- see the
/// second invariant note below), and closes at the next `TurnEnded`
/// (inclusive). A trailing segment with no closing `TurnEnded` yet is the
/// turn in progress.
///
/// **Invariant 1 (root-caused 2026-07-13 from a real, reproduced event
/// sequence -- see `docs/agent-output-ui-amendment.md`'s post-review
/// note): every span partitions the item list -- no item ever falls
/// outside every span.** This used to not hold: a user `Message`
/// unconditionally opened a *new* segment even while the previous one
/// hadn't seen a `TurnEnded` yet, closing the stale one with `ended:
/// None` on the theory that this "shouldn't happen by contract". It does
/// happen, routinely -- sending from the composer is deliberately
/// next-turn delivery even while a turn is mid-flight
/// (`docs/agent-output-ui-amendment.md` decision 6), and a user *will*
/// type another message while an earlier tool call's approval is still
/// pending (e.g. nudging a turn that looks stuck, or retrying an
/// approval that didn't seem to take effect). Each such interjection
/// used to mint a fresh span and stale the previous one forever (it can
/// never retroactively gain a `TurnEnded`) -- and once the session's
/// state eventually left the in-flight set (a cancel, or the turn
/// finally settling), the view's fallback for a dangling `ended: None`
/// span while not in flight was to render every item in it
/// *individually*, raw (`render_item`'s per-type fallback: unprocessed
/// JSON `tool`/`tool result` blocks, `Debug`-formatted `tool
/// (preparing)`, standalone approval boxes with no visible link to their
/// row) -- exactly the "incomprehensible screen state" a real session
/// hit after several rapid interjections while one bash approval sat
/// unresolved. The fix: a mid-turn interjection no longer opens a new
/// segment at all -- while a segment is already open, a further user
/// `Message` is just one more item *within* it (rendered as its own
/// message block inside the running card/receipt, via the existing
/// per-item loop in `AgentView::render_turn` -- no separate "interjection
/// row" needed for this). The segment stays open, however many messages
/// land in it, until an actual `TurnEnded` closes it -- or, if none ever
/// arrives, it's the trailing in-progress span, same as always.
///
/// **Invariant 2 (broadened 2026-07-13, same investigation): opening a
/// segment never requires a user `Message` specifically -- any item can
/// open one, as long as none is currently open.** A resumed session, or
/// a provider continuation that follows a daemon-synthesized `TurnEnded`
/// (`resume_persisted_sessions` on a `horizon-sessiond` respawn mid-turn,
/// see `docs/agent-output-ui-amendment.md`'s round-4 finding) can produce
/// tool activity or assistant text with no user `Message` immediately
/// preceding it in the frame's own item window. Requiring a `Message` to
/// open a segment left exactly this kind of item sequence permanently
/// outside every span, hitting the same raw per-item fallback invariant
/// 1 just fixed. Opening on any item closes that structural gap: the
/// implicit segment renders as the running card while a turn is
/// genuinely still in flight, and closes normally the next time a
/// `TurnEnded` arrives, same as any other span.
///
/// Note this is about *grouping* only. A separate, real production
/// sequence (session `3fe93cdb-...`, "Agent #30",
/// `hf:moonshotai/Kimi-K2.7-Code`, 2026-07-13 -- reproduced in
/// `a_batch_of_concurrent_tool_calls_with_two_overlapping_approvals_stays_one_open_span`
/// below) proved grouping alone isn't enough to guarantee the running
/// card renders: the daemon's own live `SessionState` can read a
/// non-in-flight value (`WaitingForUser`) for an extended real span of
/// time (36s in the captured log) while a batch of concurrent tool calls
/// is still resolving and a *sibling* approval is still pending --  well
/// before the span's own `TurnEnded` arrives. `AgentView::render`'s
/// per-span dispatch used to gate a dangling span's rendering vocabulary
/// on that live state reading in addition to `ended.is_none()`; it no
/// longer does -- a dangling span (by these two invariants, always the
/// turn genuinely still in progress) always renders through
/// `AgentView::render_turn`, never the flat per-item fallback, regardless
/// of what the live session state happens to read at render time.
pub(crate) fn group_into_turns(items: &[AgentFrameItem]) -> Vec<TurnSpan> {
    let mut spans = Vec::new();
    let mut current_start: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if current_start.is_none() {
            current_start = Some(index);
        }
        if let AgentFrameItem::TurnEnded {
            reason,
            model,
            elapsed,
        } = item
        {
            let start = current_start.take().unwrap_or(index);
            spans.push(TurnSpan {
                start,
                end: index + 1,
                ended: Some(TurnEnd {
                    reason: *reason,
                    model: model.clone(),
                    elapsed: *elapsed,
                }),
            });
        }
    }
    if let Some(start) = current_start {
        spans.push(TurnSpan {
            start,
            end: items.len(),
            ended: None,
        });
    }
    spans
}

/// Whether `items` contains at least one user message -- used to resolve
/// which rendered transcript block (`AgentView::render`'s `blocks`, one
/// element per turn span, plus the rare orphan-item fallback) the
/// "jump to latest user message" pill (`docs/agent-output-ui-design.md`
/// decision 7) should target. `ScrollHandle::scroll_to_top_of_item` only
/// anchors to a *direct child* of the tracked scroll container -- a whole
/// turn's rendered block, not a single message -- so `AgentView` tracks
/// the latest block containing a user message as it walks `items`,
/// calling this once per span (see `view.rs`'s `jump_to_latest_user_
/// message` doc comment for the full trade-off this approximates).
pub(crate) fn contains_user_message(items: &[AgentFrameItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            AgentFrameItem::Message(Message {
                role: MessageRole::User,
                ..
            })
        )
    })
}

/// Whether `item` is part of a tool call's lifecycle -- used by
/// [`segment_bursts`] to find burst boundaries.
fn is_tool_related(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::ToolCallRequested(_)
            | AgentFrameItem::ToolCallStarted(_)
            | AgentFrameItem::ToolCallFinished(_)
            | AgentFrameItem::ApprovalRequested(_)
            | AgentFrameItem::ToolCallPreparing(_)
    )
}

/// Whether `item` is assistant-authored text -- a streaming delta or a
/// committed assistant `Message` -- used by [`segment_bursts`].
fn is_assistant_text(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::AssistantTextDelta(_)
            | AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                ..
            })
    )
}

/// One tool burst within a turn: a maximal run of tool activity. Indices
/// are relative to the turn's own item slice (the same convention
/// [`build_tool_call_views`] uses), `[start, end)`.
///
/// Round 5 (owner decision 2026-07-13, "monotone burst splitting" --
/// superseding round 2's whole-turn provisional-receipt flip-back, see
/// `docs/agent-output-ui-amendment.md`'s post-review note): a turn can
/// fold into *more than one* receipt as it progresses -- tools run,
/// finish, the model answers, then decides to run more tools, answers
/// again, and so on. Each such run is its own burst, and a burst that
/// has closed (see [`segment_bursts`]) never reopens into a card again,
/// however much more the turn goes on to do -- eliminating the round-2
/// mechanism's "flips back to a card" bounce entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Burst {
    pub start: usize,
    pub end: usize,
    /// Whether this burst has permanently folded into a receipt.
    /// `false` only for the trailing burst of a still-running turn whose
    /// tool calls aren't all finished yet, or that has no closing
    /// assistant text after them yet -- the one burst still eligible to
    /// render as the running card. Every other burst -- including the
    /// turn's very last one, once `TurnEnded` folds -- is always `true`.
    pub closed: bool,
}

/// Segments `items` (a turn's own slice, `ended: None` or `Some` --
/// either way, from `group_into_turns`) into [`Burst`]s.
///
/// A burst opens at the first tool-related item found while none is
/// currently open, and keeps absorbing every further tool-related item
/// (however far apart, and whatever non-tool items -- an interjected
/// user message, a stray reasoning delta -- happen to fall between them)
/// until it **closes**: assistant text (a streaming delta or a committed
/// assistant `Message`) appears while every tool call opened so far in
/// the burst has finished, or the turn's own `TurnEnded` item is
/// reached. Closing is permanent -- `end` stops exactly at the last
/// absorbed tool-related item (the closing text itself is *not* part of
/// the burst; `AgentView::render_turn` renders it separately, right
/// after the burst's receipt) -- and a tool call arriving *after* that
/// closing text starts a brand new burst rather than reopening the
/// closed one. A user-message interjection never closes a burst (it
/// isn't assistant text); the burst just keeps growing through it, per
/// the same "next-turn delivery is deliberate mid-flight" reasoning
/// `group_into_turns` already documents.
///
/// A turn with no tool activity at all segments to an empty `Vec` --
/// nothing worth a receipt for; the text keeps rendering as plain
/// prose, exactly as it always has.
pub(crate) fn segment_bursts(items: &[AgentFrameItem]) -> Vec<Burst> {
    let mut bursts = Vec::new();
    let mut open: Option<(usize, usize)> = None; // (start, last_tool_index)

    for (index, item) in items.iter().enumerate() {
        if is_tool_related(item) {
            match &mut open {
                Some((_, last)) => *last = index,
                None => open = Some((index, index)),
            }
            continue;
        }
        if is_assistant_text(item) {
            if let Some((start, last)) = open {
                let all_finished = build_tool_call_views(&items[start..=last])
                    .iter()
                    .all(|call| call.finished);
                if all_finished {
                    bursts.push(Burst {
                        start,
                        end: last + 1,
                        closed: true,
                    });
                    open = None;
                }
                // Else: not closeable yet (a call opened in this burst
                // is still unfinished) -- this text isn't the closing
                // one, keep the burst open and scanning.
            }
            continue;
        }
        if matches!(item, AgentFrameItem::TurnEnded { .. }) {
            if let Some((start, last)) = open.take() {
                bursts.push(Burst {
                    start,
                    end: last + 1,
                    closed: true,
                });
            }
            // `TurnEnded` is always the turn's own last item
            // (`group_into_turns`'s invariant), so there's nothing left
            // to scan either way.
        }
        // Anything else (an interjected user `Message`, a
        // `ReasoningDelta`, `Error`, `Exited`, ...) never affects burst
        // boundaries.
    }

    if let Some((start, last)) = open {
        bursts.push(Burst {
            start,
            end: last + 1,
            closed: false,
        });
    }

    bursts
}

/// Whether a `ReasoningDelta` item outside every burst's own absorbed range
/// should render at all (owner requirement 2026-07-13: closing an
/// un-instructed deviation from base decision 5 -- thinking was completely
/// invisible while a turn ran, since `AgentView::render_turn`'s per-item
/// walk never had a match arm for it). A reasoning item that falls *inside*
/// a burst's `[start, end)` range (between two of its tool-related items,
/// "a stray reasoning delta" per [`segment_bursts`]'s own doc comment)
/// never reaches this decision at all -- it's structurally absorbed into
/// `burst_items` and dropped by `build_tool_call_views`, unaffected by this
/// fix, exactly as it always has been. For everything else (before the
/// first burst, between two bursts, after the last one, or a turn with no
/// bursts at all), visibility is simply "the turn hasn't ended yet":
/// decision 1's "thinking folds into the receipt on completion" applies
/// uniformly regardless of which of those positions a given item happened
/// to land in, so once `TurnEnded` folds, this goes back to invisible too --
/// no different from the burst-absorbed case's own fold.
pub(crate) fn thinking_visible_outside_burst(ended: Option<&TurnEnd>) -> bool {
    ended.is_none()
}

/// The receipt row's trailing content (`render_receipt`'s `tail`
/// parameter). Round 5 (monotone burst splitting): only the turn's
/// *final* burst -- the one closed by `TurnEnded` -- carries the turn's
/// end-reason status, total elapsed, and model, exactly as a completed
/// turn's one receipt always has; every other burst's receipt
/// (including the last one while the turn is still running) carries
/// none of that -- the contract has no per-burst timing, only a
/// whole-turn one. `Final` carries a `&TurnEnd` rather than duplicating
/// its fields so it can never drift from [`receipt_status`]'s own
/// reading of it.
pub(crate) enum ReceiptTail<'a> {
    Final(&'a TurnEnd),
    Intermediate,
}

/// A turn's end-reason rendered as receipt status text -- the
/// `Cancelled` -> `stopped · {elapsed}` / `Failed`/`Halted` ->
/// error-marked variants from decision 1's end-reason handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiptStatus {
    pub text: String,
    pub is_error: bool,
}

pub(crate) fn receipt_status(end: &TurnEnd) -> ReceiptStatus {
    let elapsed = humanize_duration(end.elapsed);
    match end.reason {
        TurnEndReason::Completed => ReceiptStatus {
            text: elapsed,
            is_error: false,
        },
        TurnEndReason::Cancelled => ReceiptStatus {
            text: format!("stopped · {elapsed}"),
            is_error: false,
        },
        TurnEndReason::Failed => ReceiptStatus {
            text: format!("failed · {elapsed}"),
            is_error: true,
        },
        TurnEndReason::Halted => ReceiptStatus {
            text: format!("halted · {elapsed}"),
            is_error: true,
        },
    }
}

/// Humanizes a duration the way the receipt/running-card elapsed field
/// wants it: `38s`, `2m 05s`. Whole seconds only -- sub-second precision
/// isn't meaningful at this display granularity.
pub(crate) fn humanize_duration(elapsed: Duration) -> String {
    let total_secs = elapsed.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Structured, tool-specific data a receipt chip or running-card row
/// needs beyond the generic verb/target/summary -- the file-chip
/// diffstat and the bash chip's command head (decision 1's chip
/// composition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolCallKind {
    Generic,
    File {
        file_name: String,
        /// `(added, removed)` line counts, derived from `old_string`/
        /// `new_string` for `fs.edit`. `None` when not derivable (e.g.
        /// `fs.write`, which replaces wholesale rather than diffing).
        diffstat: Option<(u32, u32)>,
    },
    Bash {
        command_head: String,
    },
}

/// One tool call's view-model, shared by the running card's per-row
/// rendering (full `verb + target + result summary` line, one row per
/// call) and the completed-turn receipt's chip rendering (terser, keyed
/// off `kind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCallView {
    pub call_id: ToolCallId,
    /// The raw tool id (e.g. `fs.edit`, `bash`) -- kept alongside the
    /// display `verb`/`kind` so receipt aggregation
    /// (`classify_call`/`aggregate_receipt`) can classify precisely
    /// without re-deriving it from display text.
    pub tool_id: String,
    pub verb: String,
    pub target: Option<String>,
    /// Set once the call has finished; a still-running call has no
    /// result to summarize yet.
    pub result_summary: Option<String>,
    pub kind: ToolCallKind,
    pub finished: bool,
    pub is_error: bool,
    /// This call's approval lifecycle (owner feedback 2026-07-13, round
    /// 3: "which tool call corresponds to which approval" -- integrating
    /// approval into the row instead of a standalone box). `None` for a
    /// call that never needed approval at all.
    pub approval: ApprovalState,
}

/// A tool call's approval lifecycle, derived in [`build_tool_call_views`]
/// from whether the call ever had an `ApprovalRequested` item and, if so,
/// how its `ToolCallStarted`/`ToolCallFinished` acks read (see [`is_denied`]
/// for the denial detection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalState {
    /// No `ApprovalRequested` item for this call at all -- an
    /// auto-approved (or never-requiring-approval) tool.
    None,
    /// An `ApprovalRequested` item exists and neither a `ToolCallStarted`
    /// nor a `ToolCallFinished` has resolved it yet -- the row still shows
    /// Approve/Deny.
    Waiting,
    /// The user approved: a `ToolCallStarted` (immediate for `bash`,
    /// alongside `ToolCallFinished` for the synchronous fs/config tools --
    /// see `crate::tools::approval::resolve_approval`'s doc comment in
    /// `crates/horizon-agent`) has folded, whether or not the call has
    /// gone on to finish yet. The daemon acks the decision one IPC hop
    /// after the click, well before a `bash` call's result -- root-caused
    /// 2026-07-13 (owner report: buttons and the proposal body lingered
    /// for the whole tool run after the click). Buttons/proposal body
    /// disappear here; the row's glyph stays ● running until
    /// `ToolCallFinished` also folds.
    Approved,
    /// The user denied: `ToolCallFinished` folded with the "denied by
    /// user" convention, with no `ToolCallStarted` at all (a deny never
    /// starts the tool).
    Denied,
}

/// Whether `result` represents the user's tool-call denial. Reads the
/// contract-explicit [`ToolCallResult::denied`] marker first -- set at the
/// source by `tools::approval::synchronous_result`'s `ran = false` path
/// (`crates/horizon-agent/src/tools/approval.rs`) -- and falls back to
/// [`is_denied_output`]'s old message-text convention only when the marker
/// reads `false`. That fallback exists for exactly one case: a
/// `ToolCallResult` persisted (as JSONL) before the marker field existed
/// deserializes with `denied: false` regardless of its real outcome
/// (`#[serde(default)]`), so replaying an old log still needs the message
/// text to classify those rows correctly. A freshly folded denial always
/// carries the marker and never needs the fallback.
fn is_denied(result: &ToolCallResult) -> bool {
    result.denied || is_denied_output(&result.output)
}

/// The old denial convention `tools::approval::denied_output` wrote for a
/// Horizon-executed tool's deny path, before [`ToolCallResult::denied`]
/// existed: `json!({ "is_error": true, "message": "denied by user" })`.
/// Checked by the message text specifically, not just `is_error`, because
/// an *approved* call that goes on to fail for its own reasons (e.g.
/// fs.edit's "old_string not found") is also `is_error: true` but carries a
/// different message -- `is_error` alone can't tell a denial from an
/// execution failure. Kept only as [`is_denied`]'s fallback for pre-marker
/// persisted logs; every current production write path sets the marker
/// instead.
fn is_denied_output(output: &Value) -> bool {
    output.get("is_error").and_then(Value::as_bool) == Some(true)
        && output.get("message").and_then(Value::as_str) == Some("denied by user")
}

/// Derives a call's [`ApprovalState`] from whether it ever had an
/// `ApprovalRequested` item and, if resolved, its `ToolCallStarted`/
/// `ToolCallFinished` acks. `started` takes priority over an absent
/// `result`: a `bash` approve folds `ToolCallStarted` immediately and its
/// `ToolCallFinished` only once the child actually exits, so a call can
/// read `Approved` here well before it reads `finished` in the same
/// [`ToolCallView`].
fn derive_approval_state(
    had_approval_request: bool,
    started: bool,
    result: Option<&ToolCallResult>,
) -> ApprovalState {
    if !had_approval_request {
        return ApprovalState::None;
    }
    match result {
        Some(result) if is_denied(result) => ApprovalState::Denied,
        Some(_) => ApprovalState::Approved,
        None if started => ApprovalState::Approved,
        None => ApprovalState::Waiting,
    }
}

/// The approval keyboard-capture state (`docs/agent-output-ui-
/// amendment.md` decision 4, stage E; re-scoped to row-centric v2 by
/// owner decision 2026-07-13): `Normal`, or targeting one specific
/// pending call for the keyboard path. Its *rendering* surface is no
/// longer a composer transformation -- stage E's banner is gone -- it's
/// now a compact "⏎ approve · esc deny" annotation on that call's own
/// row (`view::render_tool_call_row`, gated by
/// [`is_keyboard_approval_target`]). The keyboard semantics themselves
/// are unchanged: while this holds `Approval { call_id }` and the
/// composer is empty/not typing, Enter approves and Esc denies that
/// exact call; typing past it reverts to `Normal` (`next_composer_mode`'s
/// no-flap rule, below). Kept as an explicit enum -- rather than folding
/// "is approval showing" into a bool alongside a separately tracked
/// call_id -- so the amendment's own recorded future direction
/// (prompt-intent auto-approval, "auto mode") has a clean third arm to
/// add later: skip or auto-resolve this state without touching the row's
/// other paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Normal,
    Approval { call_id: ToolCallId },
}

/// Recomputes [`ComposerMode`] from the session's actionable pending
/// queue (oldest-first -- the same ordering
/// `horizon_agent::frame::actionable_pending_approval_call_ids_in`
/// returns, ghost-excluded per the round-4 post-review fix) and
/// `dismissed`: the call_id, if any, the composer most recently reverted
/// to `Normal` for because the user started typing instead of deciding.
///
/// No-flap rule (stage E): typing past a shown approval dismisses
/// *that exact call_id*, not "approval mode" in general. The composer
/// only shows `Approval` again once the queue's head actually changes --
/// either this call resolves via any of the other three paths (row
/// button, palette, CLI) and a different one takes its place, or the
/// queue was empty and gains its first entry. A queue whose head is
/// still the dismissed call_id keeps returning `Normal` here on every
/// call, however many times it's asked (e.g. once per keystroke) --
/// nothing about typing further, or deleting back to an empty composer,
/// flips it back. An empty queue always clears any dismissal along with
/// it, since there's nothing left to have dismissed.
pub(crate) fn next_composer_mode(
    actionable_queue: &[ToolCallId],
    dismissed: Option<&ToolCallId>,
) -> ComposerMode {
    match actionable_queue.first() {
        None => ComposerMode::Normal,
        Some(call_id) if Some(call_id) == dismissed => ComposerMode::Normal,
        Some(call_id) => ComposerMode::Approval {
            call_id: call_id.clone(),
        },
    }
}

/// Whether `call_id` is the exact call [`ComposerMode`] currently targets
/// for the keyboard path (row-centric v2, owner decision 2026-07-13):
/// decides which single `Waiting` row, if any, shows the "⏎ approve · esc
/// deny" annotation next to its Approve/Deny buttons. Derived purely from
/// the mode -- never from queue position -- so the hint can never lie:
/// once typing dismisses the mode back to `Normal`
/// (`next_composer_mode`'s no-flap rule), this returns `false` for every
/// call_id, including the one just shown, so the annotation disappears
/// exactly when the keys it describes stop doing anything.
pub(crate) fn is_keyboard_approval_target(mode: &ComposerMode, call_id: &ToolCallId) -> bool {
    matches!(mode, ComposerMode::Approval { call_id: target } if target == call_id)
}

/// Builds one [`ToolCallView`] per distinct tool call requested within
/// `items` (a single turn span's slice), in first-request order. A call
/// with no matching `ToolCallFinished` yet (the running turn's
/// in-flight calls) gets `finished: false` and no result summary.
pub(crate) fn build_tool_call_views(items: &[AgentFrameItem]) -> Vec<ToolCallView> {
    struct Building<'a> {
        call_id: ToolCallId,
        tool_id: &'a str,
        input: &'a Value,
        result: Option<&'a ToolCallResult>,
        had_approval_request: bool,
        started: bool,
    }

    let mut building: Vec<Building> = Vec::new();
    for item in items {
        match item {
            AgentFrameItem::ToolCallRequested(request) => {
                building.push(Building {
                    call_id: request.call_id.clone(),
                    tool_id: &request.tool_id,
                    input: &request.input,
                    result: None,
                    had_approval_request: false,
                    started: false,
                });
            }
            AgentFrameItem::ApprovalRequested(request) => {
                if let Some(entry) = building
                    .iter_mut()
                    .find(|entry| entry.call_id == request.call_id)
                {
                    entry.had_approval_request = true;
                }
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                if let Some(entry) = building.iter_mut().find(|entry| &entry.call_id == call_id) {
                    entry.started = true;
                }
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(entry) = building
                    .iter_mut()
                    .find(|entry| entry.call_id == result.call_id)
                {
                    entry.result = Some(result);
                }
            }
            _ => {}
        }
    }

    building
        .into_iter()
        .map(|entry| {
            let output = entry.result.map(|result| &result.output);
            let (verb, target, result_summary, kind) = classify(entry.tool_id, entry.input, output);
            ToolCallView {
                call_id: entry.call_id,
                tool_id: entry.tool_id.to_string(),
                verb,
                target,
                result_summary: if entry.result.is_some() {
                    result_summary
                } else {
                    None
                },
                kind,
                finished: entry.result.is_some(),
                is_error: entry.result.map(|result| result.is_error).unwrap_or(false),
                approval: derive_approval_state(
                    entry.had_approval_request,
                    entry.started,
                    entry.result,
                ),
            }
        })
        .collect()
}

/// Whether a running-card row should be click-expandable to its body
/// (`docs/agent-output-ui-design.md` decision 2: "click expands the body
/// ... collapsed is the default for every tool state including errors" --
/// stage F initially narrowed this to failed calls only for the running
/// card specifically; closed 2026-07-13 as a deviation from decision 2,
/// which never scoped the click-to-expand affordance to errors). Any
/// *finished* call qualifies, success or failure -- it expands to the same
/// per-tool body ([`tool_call_body`]) the completed-turn receipt's own
/// expansion already shows. A still-running call stays non-expandable: it
/// has no result yet to show a body for. A `Waiting` call (has an
/// unresolved approval) is also unfinished by this same rule, so it's
/// covered without a separate check -- it already auto-shows its proposal
/// body unconditionally (`AgentView::render_waiting_proposal`), untouched
/// by this predicate.
pub(crate) fn running_row_expandable(call: &ToolCallView) -> bool {
    call.finished
}

/// The composer's placeholder text (decision 6): sending from the composer
/// is always next-turn delivery, even while a turn is running (interjecting
/// into the live turn is 7b's unbuilt "steering" idea, not today's
/// behavior) -- the placeholder says so explicitly while a turn is in
/// flight, mirroring mock 7a's "続けて指示する…（送信は次のターン）".
pub(crate) fn composer_placeholder(turn_in_flight: bool) -> &'static str {
    if turn_in_flight {
        "Message the agent (sends as the next turn)…"
    } else {
        "Message the agent…"
    }
}

/// One of two inputs to the composer's model chip (see
/// [`composer_model_chip`]): the most recent `AgentFrameItem::TurnEnded`
/// that actually carries a model id, scanning `items` from the end. A
/// still-running turn's own `TurnEnded` hasn't folded yet, so it never
/// masks the previous turn's model; a completed turn that ended before any
/// provider request (`TurnEnded`'s own doc comment -- e.g. an immediate
/// cancel) is skipped in favor of an earlier turn's model, the "best
/// available value" rather than flickering the chip away. `None` until the
/// very first turn with a provider request completes.
pub(crate) fn latest_turn_model(items: &[AgentFrameItem]) -> Option<&str> {
    items.iter().rev().find_map(|item| match item {
        AgentFrameItem::TurnEnded {
            model: Some(model), ..
        } => Some(model.as_str()),
        _ => None,
    })
}

/// The composer's read-only model chip (mock's `claude-sonnet-4` pill),
/// combining the session's resolved model
/// (`agent::session::AgentSession::model`, known from session start/attach
/// -- see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum,
/// which closed the "no session-start signal" gap [`latest_turn_model`]'s
/// own doc comment used to describe) with the latest completed turn's own
/// model ([`latest_turn_model`]).
///
/// **Precedence**: `session_model` is the steady-state source of truth --
/// resolved once, synchronously, before any turn ever runs. `turn_model`
/// overrides it only when the two actively disagree, since that can only
/// mean the session's *actual* provider has moved on from what was resolved
/// at session start (there is no model switcher yet -- deferred, unbuilt
/// future work -- so this can't happen today, but the precedence is decided
/// now rather than left implicit for whenever one lands): the latest
/// completed turn is always closer to "what would happen if you sent a
/// message right now" than a possibly-stale session-start value. Falls back
/// to whichever one is `Some` if the other is `None`; `None` (chip hidden)
/// only when neither is known.
pub(crate) fn composer_model_chip<'a>(
    session_model: Option<&'a str>,
    turn_model: Option<&'a str>,
) -> Option<&'a str> {
    match (session_model, turn_model) {
        (Some(session), Some(turn)) if session != turn => Some(turn),
        (Some(session), _) => Some(session),
        (None, turn) => turn,
    }
}

/// Whether `call_id`'s approval request is still unresolved within
/// `turn_items` -- a single turn's own item slice is enough to answer
/// this without consulting the whole frame: every tool call this crate
/// emits, Horizon-executed or provider-forwarded, resolves via a
/// `ToolCallStarted` or `ToolCallFinished` with the same `call_id` (see
/// `crates/horizon-agent/src/tools/approval.rs`'s `resolve_approval`, the
/// one path every approve/deny decision funnels through -- an approve
/// folds `ToolCallStarted` immediately, `ToolCallFinished` too if the tool
/// runs synchronously; a deny folds `ToolCallFinished` alone) before its
/// turn can end in the normal case, so the resolving item -- if any --
/// already lives in the same span as the request. A turn that ends with
/// a still-pending approval (e.g. `Halted`) is the shouldn't-happen case
/// this stays `true` for, so a completed turn still renders it rather
/// than silently dropping it (`docs/agent-output-ui-amendment.md` stage
/// C's owner-reported fold bug: answered approvals must fold into the
/// receipt like any other tool activity, not linger as boxes forever).
pub(crate) fn is_approval_still_pending(
    turn_items: &[AgentFrameItem],
    call_id: &ToolCallId,
) -> bool {
    pending_approval_call_ids_in(turn_items).contains(call_id)
}

/// `(finished, total)` tool-call counts for a running card's `n / m`
/// progress header.
pub(crate) fn progress(tool_calls: &[ToolCallView]) -> (usize, usize) {
    let finished = tool_calls.iter().filter(|call| call.finished).count();
    (finished, tool_calls.len())
}

/// A tool call's class for collapsed-receipt aggregation (owner feedback
/// 2026-07-13 -- "rows of glob/grep/read chips carry no information",
/// see `docs/agent-output-ui-amendment.md`'s post-review note): `Edit`
/// and `Query` calls fold into prose counts on the receipt line; `Bash`
/// always stays individual chips (the command itself is meaningful, per
/// the owner's own framing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallClass {
    Edit,
    Bash,
    Query,
}

/// `fs.edit`/`fs.write` are `Edit`, `bash` is `Bash`, and everything
/// else -- `fs.read`/`fs.grep`/`fs.glob`/`recall.*`/`workspace.snapshot`/
/// `skill.read`/any future tool id this crate doesn't otherwise
/// recognize -- is `Query` (the "read-only, low-signal" bucket the
/// receipt aggregates, `fs.read` aside, which gets its own
/// `read_file_count`).
fn classify_call(tool_id: &str) -> CallClass {
    match tool_id {
        "fs.edit" | "fs.write" => CallClass::Edit,
        "bash" => CallClass::Bash,
        _ => CallClass::Query,
    }
}

/// The collapsed receipt line's aggregated view. `query_count` counts
/// successful `Query`-class calls *excluding* `fs.read` (which gets its
/// own `read_file_count` instead, expressed as *distinct file paths* so
/// re-reading the same file within a turn doesn't inflate the count);
/// `edited_file_count` is the same distinct-path treatment for
/// successful `Edit`-class calls; `bash_count` is the plain call count
/// for successful `Bash`-class calls (owner feedback 2026-07-13, round 3
/// follow-up: a turn with a dozen near-identical `cd … && …` bash chips
/// conveyed nothing either, the same complaint that motivated the
/// query/edit aggregation -- bash folds into prose too now).
/// `individual_calls` (any failed call of any class, plus the defensive
/// case of a call that never finished within a supposedly completed
/// turn) is the only thing left rendering as its own chip, so a failure
/// -- or an anomaly -- never goes silently missing from the collapsed
/// line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReceiptAggregate {
    pub query_count: usize,
    pub read_file_count: usize,
    pub edited_file_count: usize,
    pub bash_count: usize,
    pub individual_calls: Vec<ToolCallView>,
}

/// Aggregates `tool_calls` (a single receipt's worth, from
/// [`build_tool_call_views`]) into a [`ReceiptAggregate`]. Order within
/// `individual_calls` follows `tool_calls`' own (first-request) order.
pub(crate) fn aggregate_receipt(tool_calls: &[ToolCallView]) -> ReceiptAggregate {
    let mut aggregate = ReceiptAggregate::default();
    let mut read_paths: HashSet<String> = HashSet::new();
    let mut edited_paths: HashSet<String> = HashSet::new();

    for call in tool_calls {
        if call.is_error || !call.finished {
            // A failed call never aggregates, regardless of class (the
            // owner's explicit requirement) -- nor does the defensive
            // "never finished within a completed turn" case, which
            // shouldn't happen by contract but must not silently vanish
            // into a count either.
            aggregate.individual_calls.push(call.clone());
            continue;
        }
        match classify_call(&call.tool_id) {
            CallClass::Edit => {
                if let Some(path) = &call.target {
                    edited_paths.insert(path.clone());
                }
            }
            CallClass::Bash => aggregate.bash_count += 1,
            CallClass::Query if call.tool_id == "fs.read" => {
                if let Some(path) = &call.target {
                    read_paths.insert(path.clone());
                }
            }
            CallClass::Query => aggregate.query_count += 1,
        }
    }

    aggregate.read_file_count = read_paths.len();
    aggregate.edited_file_count = edited_paths.len();
    aggregate
}

/// `1 {singular}` / `{count} {plural}`.
fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// The collapsed receipt line's prose prefix (owner feedback
/// 2026-07-13): `None` when every aggregated count is zero (e.g. an
/// all-individual-chips turn), so the line never shows a hollow "0 tool
/// calls" -- it just goes straight to whatever chips/status/model
/// follow.
pub(crate) fn receipt_prose(aggregate: &ReceiptAggregate) -> Option<String> {
    let mut parts = Vec::new();
    if aggregate.query_count > 0 {
        parts.push(pluralize(aggregate.query_count, "tool call", "tool calls"));
    }
    if aggregate.read_file_count > 0 {
        parts.push(format!(
            "read {}",
            pluralize(aggregate.read_file_count, "file", "files")
        ));
    }
    if aggregate.edited_file_count > 0 {
        parts.push(format!(
            "edited {}",
            pluralize(aggregate.edited_file_count, "file", "files")
        ));
    }
    if aggregate.bash_count > 0 {
        parts.push(format!(
            "ran {}",
            pluralize(aggregate.bash_count, "command", "commands")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// One file's cumulative edit/write activity across the *whole session*
/// (every turn, not just whichever receipt/burst is currently rendering)
/// -- the pane's collapsible "Changes overview"
/// (`docs/agent-output-ui-design.md` decision 9, never ported from the
/// retired Floem shell's own `session_changes` pure function; rebuilt
/// fresh here against this shell's own `ToolCallView`/
/// `build_tool_call_views` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub path: String,
    pub file_name: String,
    pub added: u32,
    pub removed: u32,
    /// Set once any successful `fs.write` call against this path reported
    /// `created: true` (the same `output.get("created")` convention
    /// `classify`'s `fs.write` arm reads). `fs.write` never produces a
    /// diffstat at all (`ToolCallKind::File::diffstat` is `None` for it --
    /// it replaces wholesale rather than diffing), so this flag is the
    /// overview's only signal from a write call.
    pub created: bool,
}

/// Aggregates every successful, finished `fs.edit`/`fs.write` call in
/// `tool_calls` -- the *whole session's* [`build_tool_call_views`] output,
/// not one turn/burst's -- into one [`FileChange`] per distinct path,
/// ordered by each path's first touch. A failed call (`is_error`) or one
/// still in flight contributes nothing, the same "failures never
/// aggregate" rule [`aggregate_receipt`] follows -- simplified here to a
/// plain skip, since this overview has no per-call chip fallback to fall
/// back to.
///
/// **Honest limitation**: multiple edits to the same file have their
/// diffstats *summed*, not combined into a net diff across the file's
/// whole session history -- two edits that each touch 3 lines report `+6
/// −6` here even if the second fully reverted the first's changes.
/// [`ToolCallKind::File::diffstat`] is itself only a per-call
/// reconstruction (`reconstruct_line_diff`'s common-prefix/common-suffix
/// approximation against that one call's own `old_string`/`new_string`),
/// and this aggregation has no access to the file's real end-to-end
/// content to diff against instead.
pub(crate) fn aggregate_changes(tool_calls: &[ToolCallView]) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    for call in tool_calls {
        if call.is_error || !call.finished || classify_call(&call.tool_id) != CallClass::Edit {
            continue;
        }
        let Some(path) = &call.target else {
            continue;
        };
        let entry = match changes.iter_mut().find(|change| &change.path == path) {
            Some(entry) => entry,
            None => {
                changes.push(FileChange {
                    path: path.clone(),
                    file_name: file_name(path),
                    added: 0,
                    removed: 0,
                    created: false,
                });
                changes.last_mut().expect("just pushed")
            }
        };
        if let ToolCallKind::File {
            diffstat: Some((added, removed)),
            ..
        } = &call.kind
        {
            entry.added += added;
            entry.removed += removed;
        }
        if call.tool_id == "fs.write" && call.result_summary.as_deref() == Some("created") {
            entry.created = true;
        }
    }
    changes
}

/// The Changes overview bar's summary text (decision 9): `None` when no
/// file was ever edited/written this session -- the bar itself is hidden
/// entirely in that case (the view gates on this, not a separate emptiness
/// check on [`aggregate_changes`]'s own output, so the two can never
/// drift). `+`/`−` counts sum every aggregated file's own diffstat,
/// inheriting [`aggregate_changes`]'s documented "summed hunk stats, not a
/// net diff" limitation.
pub(crate) fn changes_summary_text(changes: &[FileChange]) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let added: u32 = changes.iter().map(|change| change.added).sum();
    let removed: u32 = changes.iter().map(|change| change.removed).sum();
    Some(format!(
        "{} · +{added} −{removed}",
        pluralize(changes.len(), "file", "files")
    ))
}

/// One `todo.write` item's view-model (`docs/agent-todo-tool-design.md`
/// decision 2), mirroring the tool's own validated shape
/// (`crates/horizon-agent/src/tools/todo.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

/// Derives the session's current plan/todo list (`docs/agent-todo-tool-
/// design.md` decision 3): walks every `todo.write` call across the
/// *whole session's* `items` (not one turn's slice), in event order, and
/// keeps the last one that finished without error -- `todo.write`
/// replaces the whole list every call, so "latest wins" is the entire
/// fold; there is nothing to merge across calls. A call still in flight,
/// or one that failed validation, contributes nothing, the same
/// "failures never aggregate" rule [`aggregate_changes`] follows. `None`
/// when no successful `todo.write` call has ever landed -- the panel is
/// hidden entirely in that case. An explicit empty list (`items: []`,
/// e.g. the agent clearing a finished plan) also folds to zero items and
/// therefore hides the panel too via [`todo_summary_text`]'s own
/// emptiness check -- deliberately not distinguished from "never
/// called", the same simplification [`aggregate_changes`]/
/// [`changes_summary_text`] make for a session that never touched a
/// file.
///
/// Implemented as its own direct walk over [`AgentFrameItem`] rather than
/// layered on [`ToolCallView`]/`classify` the way [`aggregate_changes`]
/// is: adding a `todo.write` arm to `classify` would need a new
/// `ToolCallKind` variant, which `view.rs`'s `render_receipt_chip` (an
/// exhaustive match over `ToolCallKind`) would then need a new arm for
/// too -- reaching into the transcript's shared row/receipt rendering,
/// which this feature deliberately leaves untouched. This derivation
/// stays fully self-contained in this module instead, pairing
/// `ToolCallRequested`/`ToolCallFinished` by call id itself.
pub(crate) fn latest_todo_list(items: &[AgentFrameItem]) -> Option<Vec<TodoItem>> {
    struct Requested<'a> {
        call_id: &'a ToolCallId,
        input: &'a Value,
    }

    let mut requested: Vec<Requested> = Vec::new();
    let mut latest: Option<Vec<TodoItem>> = None;
    for item in items {
        match item {
            AgentFrameItem::ToolCallRequested(request) if request.tool_id == "todo.write" => {
                requested.push(Requested {
                    call_id: &request.call_id,
                    input: &request.input,
                });
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(position) = requested
                    .iter()
                    .position(|entry| entry.call_id == &result.call_id)
                {
                    let entry = requested.remove(position);
                    if !result.is_error {
                        latest = Some(todo_items_from_input(entry.input));
                    }
                }
            }
            _ => {}
        }
    }
    latest
}

/// Parses a `todo.write` request's `items` array into view-model items,
/// silently dropping any entry that doesn't match the tool's own
/// validated shape (defensive -- a successful call already passed
/// `tools::todo::execute`'s validation, so this should never actually
/// drop anything in practice).
fn todo_items_from_input(input: &Value) -> Vec<TodoItem> {
    input
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let text = item.get("text").and_then(Value::as_str)?.to_string();
            let status = match item.get("status").and_then(Value::as_str)? {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "done" => TodoStatus::Done,
                _ => return None,
            };
            Some(TodoItem { text, status })
        })
        .collect()
}

/// The Plan panel's collapsed-row summary (`docs/agent-todo-tool-
/// design.md` decision 5): `None` when the list is empty, hiding the
/// panel entirely -- gating callers use this, not a separate emptiness
/// check on [`latest_todo_list`]'s own output, so the two can never
/// drift (the same discipline [`changes_summary_text`]'s own doc comment
/// follows).
pub(crate) fn todo_summary_text(items: &[TodoItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let done = items
        .iter()
        .filter(|item| item.status == TodoStatus::Done)
        .count();
    Some(format!("{done}/{} done", items.len()))
}

/// Maps a tool id to its display verb, target, (would-be) result
/// summary, and any tool-specific structured data -- the one place that
/// knows the exact input/output JSON shape each tool in
/// `crates/horizon-agent/src/tools` uses (see that crate's `tools/fs`,
/// `tools/bash` modules). Unknown tool ids fall back to the raw id as
/// the verb with no target/summary, so a future tool renders *something*
/// sane rather than nothing.
fn classify(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> (String, Option<String>, Option<String>, ToolCallKind) {
    match tool_id {
        "fs.edit" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let old = str_field(input, "old_string").unwrap_or_default();
            let new = str_field(input, "new_string").unwrap_or_default();
            let diffstat = Some(line_diffstat(old, new));
            let summary = diffstat.map(|(added, removed)| format!("+{added} -{removed}"));
            (
                "Edit".to_string(),
                Some(path.clone()),
                summary,
                ToolCallKind::File {
                    file_name: file_name(&path),
                    diffstat,
                },
            )
        }
        "fs.write" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("created"))
                .and_then(Value::as_bool)
                .map(|created| {
                    if created {
                        "created".to_string()
                    } else {
                        "overwritten".to_string()
                    }
                });
            (
                "Write".to_string(),
                Some(path.clone()),
                summary,
                ToolCallKind::File {
                    file_name: file_name(&path),
                    diffstat: None,
                },
            )
        }
        "bash" => {
            let command = str_field(input, "command").unwrap_or_default();
            let head = command_head(command);
            let summary = output
                .and_then(|output| output.get("exit_code"))
                .and_then(Value::as_i64)
                .map(|code| format!("exit {code}"));
            (
                "Bash".to_string(),
                Some(head.clone()),
                summary,
                ToolCallKind::Bash { command_head: head },
            )
        }
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("total_lines"))
                .and_then(Value::as_u64)
                .map(|lines| format!("{lines} lines"));
            (
                "Read".to_string(),
                Some(path),
                summary,
                ToolCallKind::Generic,
            )
        }
        "fs.grep" => {
            let pattern = str_field(input, "pattern").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64)
                .map(|count| format!("{count} matches"));
            (
                "Grep".to_string(),
                Some(pattern),
                summary,
                ToolCallKind::Generic,
            )
        }
        "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64)
                .map(|count| format!("{count} matches"));
            (
                "Glob".to_string(),
                Some(pattern),
                summary,
                ToolCallKind::Generic,
            )
        }
        "workspace.snapshot" => ("Snapshot".to_string(), None, None, ToolCallKind::Generic),
        "config.read" => ("Config Read".to_string(), None, None, ToolCallKind::Generic),
        "config.write" => (
            "Config Write".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.search" => (
            "Recall Search".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.read" => ("Recall Read".to_string(), None, None, ToolCallKind::Generic),
        "skill.read" => {
            let id = str_field(input, "id").unwrap_or_default().to_string();
            ("Skill".to_string(), Some(id), None, ToolCallKind::Generic)
        }
        other => (other.to_string(), None, None, ToolCallKind::Generic),
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

/// First line of `command`, truncated to a display-friendly length.
fn command_head(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or("");
    truncate_chars(first_line, 32)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// A simple common-prefix/common-suffix line diffstat between `old` and
/// `new` -- not a full diff algorithm (no interior-line matching), but
/// enough to report `+added -removed` for `fs.edit`'s single
/// old_string/new_string replacement, which is the shape every `fs.edit`
/// call has today (see `crates/horizon-agent/src/tools/fs/edit.rs`).
/// Derived from [`reconstruct_line_diff`] rather than computed
/// independently, so the receipt chip's counts and the expanded body's
/// diff can never drift apart.
fn line_diffstat(old: &str, new: &str) -> (u32, u32) {
    let lines = reconstruct_line_diff(old, new);
    let added = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Added)
        .count() as u32;
    let removed = lines
        .iter()
        .filter(|line| line.kind == DiffLineKind::Removed)
        .count() as u32;
    (added, removed)
}

/// One line of a reconstructed diff body (stage D's fs.edit expansion,
/// `docs/agent-output-ui-design.md` decision 4): `Context` lines are the
/// common prefix/suffix trimmed below, painted with neither role;
/// `Added`/`Removed` pair with `theme::diff_added_*`/`diff_removed_*` in
/// the view (line background carries the change, sign column colored
/// separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Reconstructs a full line diff between `old` and `new` by trimming the
/// common prefix/suffix (kept as `Context` lines) and pairing the
/// remaining middle as removed-then-added -- not a full diff algorithm
/// (no interior-line matching), matching `fs.edit`'s single
/// old_string/new_string replacement shape. Operates on `&str` lines
/// throughout, so multibyte content (e.g. Japanese text) round-trips
/// unmodified -- no byte-level slicing here.
fn reconstruct_line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut lines = Vec::new();
    for text in &old_lines[..prefix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[prefix..old_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Removed,
            text: (*text).to_string(),
        });
    }
    for text in &new_lines[prefix..new_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[old_lines.len() - suffix..] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    lines
}

/// A tool call's expanded-row body (stage D, decision 3's "each row
/// expands further individually"), keyed off the tool id the same way
/// [`ToolCallKind`] is. Every line-list variant is already height-capped
/// by [`build_tool_call_body`]; the view additionally wraps them in a
/// scrollable, height-bounded container so one body can't swallow the
/// transcript. Deliberately reusable beyond the receipt: stage F's
/// failed-call log (running-card row) wants the same per-tool body
/// machinery, so this and [`tool_call_body`] take a plain item slice +
/// call id rather than anything receipt-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolCallBody {
    /// fs.edit -- a reconstructed line diff; `omitted` counts any lines
    /// trimmed by the cap.
    Diff {
        lines: Vec<DiffLine>,
        omitted: usize,
    },
    /// fs.write -- a content preview labeled created/overwritten from the
    /// output, head-capped (the start of a new file matters most).
    ContentPreview {
        label: String,
        lines: Vec<String>,
        omitted: usize,
    },
    /// bash -- the command, its exit code (when the call didn't error
    /// before producing one), and captured output, tail-capped (the
    /// final pass/fail summary matters most -- mirrors
    /// `tools::bash::output::cap`'s own head/tail trade-off note).
    Command {
        command: String,
        exit_code: Option<i64>,
        lines: Vec<String>,
        omitted: usize,
    },
    /// fs.read/glob/grep and other known-but-terse tools -- one summary
    /// line (path + range, match counts, ...).
    Summary(String),
    /// An unrecognized tool id -- the base design's raw-JSON fallback,
    /// pretty-printed and head-capped.
    Raw { lines: Vec<String>, omitted: usize },
}

/// Diff body line cap -- generous, since a single `fs.edit` replacement is
/// normally small; guards against an unusually large one still bounding
/// the number of elements the view has to build.
const MAX_DIFF_LINES: usize = 300;
/// fs.write content-preview line cap (head-capped: the file's start
/// matters most for a preview).
const CONTENT_PREVIEW_MAX_LINES: usize = 200;
/// bash captured-output line cap (tail-capped: the final summary line
/// matters most, see `ToolCallBody::Command`'s doc comment).
const BASH_OUTPUT_TAIL_LINES: usize = 100;
/// Raw-JSON-fallback line cap (head-capped).
const RAW_FALLBACK_MAX_LINES: usize = 200;

/// Caps `lines` to its first `max_lines` entries, returning `(kept,
/// omitted)` -- used wherever the head of the content matters most (diff
/// bodies, content previews, the raw-JSON fallback).
fn cap_lines_head<T>(mut lines: Vec<T>, max_lines: usize) -> (Vec<T>, usize) {
    if lines.len() <= max_lines {
        (lines, 0)
    } else {
        let omitted = lines.len() - max_lines;
        lines.truncate(max_lines);
        (lines, omitted)
    }
}

/// Caps `lines` to its last `max_lines` entries -- used for bash output,
/// where the tail (the final pass/fail summary) matters most.
fn cap_lines_tail(mut lines: Vec<String>, max_lines: usize) -> (Vec<String>, usize) {
    if lines.len() <= max_lines {
        (lines, 0)
    } else {
        let omitted = lines.len() - max_lines;
        let kept = lines.split_off(lines.len() - max_lines);
        (kept, omitted)
    }
}

/// A streaming reasoning ("thinking") block's line cap -- kept small,
/// deliberately quieter and more compact than a tool-call body's own caps
/// (`BASH_OUTPUT_TAIL_LINES` etc.): thinking is meant to read as a quiet
/// side-channel while it streams, not a large panel competing with
/// assistant prose for the transcript's vertical space.
pub(crate) const THINKING_TAIL_LINES: usize = 6;

/// Caps a streaming `ReasoningDelta`'s accumulated text to its trailing
/// [`THINKING_TAIL_LINES`]-shaped view (owner requirement 2026-07-13:
/// height-bounded, newest content visible, so a long thinking stream can't
/// flood the transcript while it's the only thing on screen during an
/// otherwise-idle wait). `text` is the item's own coalesced field --
/// `frame.rs`'s `Event::ReasoningDelta` fold appends every delta of one
/// reasoning span into a single growing `.text`, so this runs fresh on
/// every render of a still-streaming block, not once per delta -- splits on
/// `\n` and reuses [`cap_lines_tail`] (the same "tail matters most" shape
/// bash output already gets), the simplest bound consistent with the rest
/// of this module's line-based caps. Returns the kept text rejoined with
/// `\n`, and the count of leading lines dropped (0 when it already fits).
pub(crate) fn cap_thinking_text(text: &str, max_lines: usize) -> (String, usize) {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (kept, omitted) = cap_lines_tail(lines, max_lines);
    (kept.join("\n"), omitted)
}

/// The tool ids [`classify`] gives a dedicated verb/target/summary to --
/// shared with [`build_tool_call_body`] so a genuinely unrecognized tool
/// id (a future tool this crate hasn't been taught about yet) still falls
/// back to the raw-JSON body rather than a blank one, per decision 3's
/// "raw JSON pretty-print only as the unknown-tool fallback".
fn is_known_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "fs.edit"
            | "fs.write"
            | "bash"
            | "fs.read"
            | "fs.grep"
            | "fs.glob"
            | "workspace.snapshot"
            | "config.read"
            | "config.write"
            | "recall.search"
            | "recall.read"
            | "skill.read"
    )
}

/// A terse one-line summary for a known-but-not-specially-bodied tool
/// call. fs.read/grep/glob get shapes derived from their actual output
/// JSON (see `crates/horizon-agent/src/tools/fs/{read,grep,glob}.rs`);
/// every other known tool id falls back to [`classify`]'s own
/// verb/target/summary, reused rather than duplicated.
fn terse_summary(tool_id: &str, input: &Value, output: Option<&Value>) -> String {
    match tool_id {
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default();
            let range = output.and_then(|output| {
                let start = output.get("start_line").and_then(Value::as_u64)?;
                let end = output.get("end_line").and_then(Value::as_u64)?;
                let total = output.get("total_lines").and_then(Value::as_u64)?;
                Some(format!("lines {start}-{end} of {total}"))
            });
            match range {
                Some(range) => format!("{path} · {range}"),
                None => path.to_string(),
            }
        }
        "fs.grep" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64);
            match count {
                Some(count) => format!("\"{pattern}\" in {base} · {count} matches"),
                None => format!("\"{pattern}\" in {base}"),
            }
        }
        "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64);
            match count {
                Some(count) => format!("{pattern} in {base} · {count} matches"),
                None => format!("{pattern} in {base}"),
            }
        }
        _ => {
            let (verb, target, result_summary, _kind) = classify(tool_id, input, output);
            match (target, result_summary) {
                (Some(target), Some(summary)) => format!("{verb} {target} · {summary}"),
                (Some(target), None) => format!("{verb} {target}"),
                (None, Some(summary)) => format!("{verb} · {summary}"),
                (None, None) => verb,
            }
        }
    }
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Builds the raw-JSON fallback body's lines for a tool id [`classify`]
/// doesn't recognize.
fn raw_json_fallback(tool_id: &str, input: &Value, output: Option<&Value>) -> (Vec<String>, usize) {
    let mut text = format!("{tool_id}\ninput: {}", pretty_json(input));
    if let Some(output) = output {
        text.push_str(&format!("\noutput: {}", pretty_json(output)));
    }
    cap_lines_head(
        text.lines().map(str::to_string).collect(),
        RAW_FALLBACK_MAX_LINES,
    )
}

/// Maps a tool call's id/input/(optional) output to its [`ToolCallBody`]
/// -- the per-tool body renderers of decision 3: fs.edit gets a
/// reconstructed diff, fs.write a content preview, bash a command+output
/// block, and every other known tool id a terse summary; a truly unknown
/// id falls back to raw JSON.
pub(crate) fn build_tool_call_body(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> ToolCallBody {
    match tool_id {
        "fs.edit" => {
            let old = str_field(input, "old_string").unwrap_or_default();
            let new = str_field(input, "new_string").unwrap_or_default();
            let (lines, omitted) = cap_lines_head(reconstruct_line_diff(old, new), MAX_DIFF_LINES);
            ToolCallBody::Diff { lines, omitted }
        }
        "fs.write" => {
            let label = output
                .and_then(|output| output.get("created"))
                .and_then(Value::as_bool)
                .map(|created| if created { "created" } else { "overwritten" })
                .unwrap_or("written")
                .to_string();
            let content = str_field(input, "content").unwrap_or_default();
            let (lines, omitted) = cap_lines_head(
                content.lines().map(str::to_string).collect(),
                CONTENT_PREVIEW_MAX_LINES,
            );
            ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            }
        }
        "bash" => {
            let command = str_field(input, "command").unwrap_or_default().to_string();
            let exit_code = output
                .and_then(|output| output.get("exit_code"))
                .and_then(Value::as_i64);
            let output_text = output
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let all_lines: Vec<String> = output_text.lines().map(str::to_string).collect();
            let (lines, omitted) = cap_lines_tail(all_lines, BASH_OUTPUT_TAIL_LINES);
            ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            }
        }
        _ if is_known_tool_id(tool_id) => {
            ToolCallBody::Summary(terse_summary(tool_id, input, output))
        }
        _ => {
            let (lines, omitted) = raw_json_fallback(tool_id, input, output);
            ToolCallBody::Raw { lines, omitted }
        }
    }
}

/// Finds `call_id`'s request/result within `items` (a single turn's item
/// slice, same contract as [`build_tool_call_views`]) and builds its
/// [`ToolCallBody`]. `None` if `call_id` has no matching request in
/// `items` at all (shouldn't happen for a row the caller already built a
/// [`ToolCallView`] from).
pub(crate) fn tool_call_body(
    items: &[AgentFrameItem],
    call_id: &ToolCallId,
) -> Option<ToolCallBody> {
    let request = items.iter().find_map(|item| match item {
        AgentFrameItem::ToolCallRequested(request) if &request.call_id == call_id => Some(request),
        _ => None,
    })?;
    let result = items.iter().find_map(|item| match item {
        AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id => Some(result),
        _ => None,
    });
    Some(build_tool_call_body(
        &request.tool_id,
        &request.input,
        result.map(|result| &result.output),
    ))
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::{
        ApprovalRequest, MessageDelta, ToolCallId, ToolCallRequest, ToolCallResult,
    };
    use serde_json::json;

    use super::*;

    fn user_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            text: text.to_string(),
        })
    }

    fn assistant_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn assistant_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn reasoning_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::ReasoningDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn tool_requested(call_id: &str, tool_id: &str, input: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: tool_id.to_string(),
            input,
        })
    }

    fn tool_finished(call_id: &str, output: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            output,
        ))
    }

    fn tool_started(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallStarted(ToolCallId(call_id.to_string()))
    }

    fn turn_ended(reason: TurnEndReason, model: Option<&str>, elapsed_secs: u64) -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason,
            model: model.map(str::to_string),
            elapsed: Duration::from_secs(elapsed_secs),
        }
    }

    #[test]
    fn groups_a_completed_turn_followed_by_a_running_one() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested(
                "a",
                "fs.grep",
                json!({"base_path": ".", "pattern": "notify"}),
            ),
            tool_finished("a", json!({"returned_count": 1})),
            assistant_message("fixed it"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 38),
            user_message("check the other form too"),
            tool_requested("b", "fs.read", json!({"path": "signup_form.rs"})),
        ];

        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 2);

        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 5); // inclusive of TurnEnded
        let ended = spans[0].ended.as_ref().expect("first turn settled");
        assert_eq!(ended.reason, TurnEndReason::Completed);
        assert_eq!(ended.model.as_deref(), Some("gpt-5"));
        assert_eq!(ended.elapsed, Duration::from_secs(38));

        assert_eq!(spans[1].start, 5);
        assert_eq!(spans[1].end, 7);
        assert!(spans[1].ended.is_none());
    }

    #[test]
    fn a_turn_with_no_tool_calls_still_groups_and_has_no_chips() {
        let items = vec![
            user_message("hello"),
            assistant_message("hi"),
            turn_ended(TurnEndReason::Completed, None, 2),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert!(span.ended.is_some());
        assert!(build_tool_call_views(&items[span.start..span.end]).is_empty());
    }

    #[test]
    fn a_second_user_message_with_no_turn_ended_between_them_merges_into_one_open_span() {
        // Root-caused 2026-07-13: a mid-turn interjection (the user
        // typing again before the previous turn closed) must not orphan
        // the first message into a permanently-dangling span -- it's
        // just one more item inside the still-open one.
        let items = vec![user_message("first"), user_message("second")];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: 2,
                ended: None,
            }]
        );
    }

    #[test]
    fn a_mid_turn_interjection_while_an_approval_is_pending_stays_in_the_same_open_span() {
        // Reproduces the real event sequence behind the owner's
        // 2026-07-13 "partial approve leads to an incomprehensible
        // screen state" report: the user sent a message while an earlier
        // bash call's approval was still unresolved, the model retried
        // the same bash call (a *second* unresolved approval), and the
        // user interjected again -- and again. Multiple interjections
        // must not fragment this into several dangling spans.
        let items = vec![
            user_message("一旦このMVPでよいです。"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            approval_requested("a"),
            // "a" is never resolved -- the user, unable to tell whether
            // approving it worked, interjects instead of waiting.
            user_message("a"),
            tool_requested("b", "bash", json!({"command": "cargo build"})),
            approval_requested("b"),
            user_message("なんかapprove出来ないな"),
            tool_requested("c", "bash", json!({"command": "cargo build"})),
            approval_requested("c"),
            user_message("だから出来ないって言ってるでしょ"),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: items.len(),
                ended: None,
            }]
        );
    }

    #[test]
    fn the_interjection_span_closes_normally_once_a_turn_ended_finally_arrives() {
        // Continuing the same reproduction: eventually the turn is
        // cancelled, which finally closes the whole merged span -- every
        // interjection and its tool calls fold into one receipt, not
        // several dangling ones.
        let items = vec![
            user_message("一旦このMVPでよいです。"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            approval_requested("a"),
            user_message("a"),
            tool_requested("b", "bash", json!({"command": "cargo build"})),
            approval_requested("b"),
            turn_ended(TurnEndReason::Cancelled, None, 42),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, items.len());
        let ended = spans[0].ended.as_ref().expect("closed by the TurnEnded");
        assert_eq!(ended.reason, TurnEndReason::Cancelled);
    }

    #[test]
    fn a_batch_of_concurrent_tool_calls_with_two_overlapping_approvals_stays_one_open_span() {
        // Reproduces the real event sequence behind the owner's
        // 2026-07-13 "approving the FORMER of two pending approvals
        // breaks the layout as attached" report (session
        // `3fe93cdb-3119-409d-8da7-b4c53c0883bf`, pane title "Agent #30",
        // `hf:moonshotai/Kimi-K2.7-Code`, reconstructed from
        // `~/.local/share/horizon/agent-events.jsonl`). The model issued
        // a batch of tool calls within one turn: a snapshot and several
        // `fs.read`s that never need approval, interleaved with three
        // `bash` calls that do -- the last two (`bash:7`/`bash:8`)
        // requested back-to-back before either resolved, exactly the
        // "two approvals showing" moment from the screenshot. The
        // daemon's own `SessionState` read `WaitingForUser` for a real
        // 36-second span between resolving `bash:7`'s approval and
        // starting `bash:8`'s -- `state_indicates_turn_in_flight` is
        // false for `WaitingForUser` -- but the *item* sequence itself
        // never gets a `TurnEnded` until everything settles. This
        // confirms grouping was never the bug for this case: it already
        // produces one continuous open span throughout, exactly as
        // asserted below. The actual root cause was
        // `AgentView::render`'s per-span dispatch additionally gating a
        // dangling span's rendering vocabulary on that live state
        // reading -- see `group_into_turns`'s invariant 2 note and
        // `AgentView::render`'s span walk, which no longer does that.
        let items = vec![
            user_message("このリポジトリの内容を把握してください"),
            tool_requested("workspace.snapshot:0", "workspace.snapshot", json!({})),
            tool_finished("workspace.snapshot:0", json!({"tab_count": 2})),
            tool_requested("bash:1", "bash", json!({"command": "ls -la"})),
            approval_requested("bash:1"),
            // Auto-approved siblings proceed immediately even while
            // `bash:1`'s approval is still unresolved -- the runtime
            // doesn't block the whole batch on one pending decision.
            tool_requested("fs.read:2", "fs.read", json!({"path": "README.md"})),
            tool_finished("fs.read:2", json!({"total_lines": 107})),
            tool_requested("fs.read:3", "fs.read", json!({"path": "Cargo.toml"})),
            tool_finished("fs.read:3", json!({"total_lines": 74})),
            tool_finished("bash:1", json!({"exit_code": 0})),
            tool_requested("bash:6", "bash", json!({"command": "ls src"})),
            approval_requested("bash:6"),
            tool_finished("bash:6", json!({"exit_code": 0})),
            tool_requested("fs.read:4", "fs.read", json!({"path": "docs/roadmap.md"})),
            tool_finished("fs.read:4", json!({"total_lines": 50})),
            tool_requested("fs.read:5", "fs.read", json!({"path": "AGENTS.md"})),
            tool_finished("fs.read:5", json!({"total_lines": 200})),
            // The two overlapping approvals: both requested before
            // either resolves.
            tool_requested("bash:7", "bash", json!({"command": "find . -maxdepth 2"})),
            approval_requested("bash:7"),
            tool_requested("bash:8", "bash", json!({"command": "cargo metadata"})),
            approval_requested("bash:8"),
            // The owner approves the FORMER (`bash:7`) first -- `bash:8`
            // stays pending for a long real-world gap (36s in the actual
            // log, invisible to grouping since it operates on items, not
            // timestamps) before it, too, resolves.
            tool_finished("bash:7", json!({"exit_code": 0})),
            tool_finished("bash:8", json!({"exit_code": 0})),
            assistant_message("Here's what I found in the repository..."),
            turn_ended(
                TurnEndReason::Completed,
                Some("hf:moonshotai/Kimi-K2.7-Code"),
                54,
            ),
        ];

        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, items.len());
        let ended = spans[0].ended.as_ref().expect("closed by the TurnEnded");
        assert_eq!(ended.reason, TurnEndReason::Completed);
    }

    #[test]
    fn a_turn_opening_item_that_is_not_a_user_message_still_opens_a_span() {
        // Invariant 2 (broadened 2026-07-13): a structural gap -- e.g. a
        // provider continuation following a daemon-synthesized
        // `TurnEnded` on a `horizon-sessiond` respawn mid-turn
        // (`docs/agent-output-ui-amendment.md`'s round-4 finding) -- can
        // leave tool activity or assistant text with no user `Message`
        // immediately preceding it in the frame's own item window.
        // Before this fix, only a user `Message` could open a segment,
        // so this item sequence fell entirely outside every span and hit
        // the raw per-item fallback despite being ordinary tool
        // activity.
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "README.md"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_message("done"),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(
            spans,
            vec![TurnSpan {
                start: 0,
                end: items.len(),
                ended: None,
            }]
        );
    }

    #[test]
    fn receipt_status_covers_every_end_reason() {
        let end = |reason| TurnEnd {
            reason,
            model: None,
            elapsed: Duration::from_secs(38),
        };
        assert_eq!(
            receipt_status(&end(TurnEndReason::Completed)),
            ReceiptStatus {
                text: "38s".to_string(),
                is_error: false
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Cancelled)),
            ReceiptStatus {
                text: "stopped · 38s".to_string(),
                is_error: false
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Failed)),
            ReceiptStatus {
                text: "failed · 38s".to_string(),
                is_error: true
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Halted)),
            ReceiptStatus {
                text: "halted · 38s".to_string(),
                is_error: true
            }
        );
    }

    #[test]
    fn humanize_duration_matches_the_docs_examples() {
        assert_eq!(humanize_duration(Duration::from_secs(0)), "0s");
        assert_eq!(humanize_duration(Duration::from_secs(38)), "38s");
        assert_eq!(humanize_duration(Duration::from_secs(59)), "59s");
        assert_eq!(humanize_duration(Duration::from_secs(60)), "1m 00s");
        assert_eq!(humanize_duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn build_tool_call_views_pairs_requests_with_their_results_in_request_order() {
        let items = vec![
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_requested("b", "fs.read", json!({"path": "src/lib.rs"})),
            tool_finished("a", json!({"returned_count": 3})),
            tool_finished("b", json!({"total_lines": 40})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].call_id, ToolCallId("a".to_string()));
        assert_eq!(views[0].verb, "Grep");
        assert_eq!(views[0].result_summary.as_deref(), Some("3 matches"));
        assert!(views[0].finished);
        assert!(!views[0].is_error);

        assert_eq!(views[1].call_id, ToolCallId("b".to_string()));
        assert_eq!(views[1].verb, "Read");
        assert_eq!(views[1].result_summary.as_deref(), Some("40 lines"));
    }

    #[test]
    fn a_still_running_tool_call_has_no_result_summary() {
        let items = vec![tool_requested(
            "a",
            "bash",
            json!({"command": "cargo test"}),
        )];
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 1);
        assert!(!views[0].finished);
        assert!(views[0].result_summary.is_none());
        assert!(!views[0].is_error);
    }

    #[test]
    fn an_errored_tool_call_is_marked_is_error_via_the_output_convention() {
        let items = vec![
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished(
                "a",
                json!({"is_error": true, "message": "boom", "exit_code": 1}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert!(views[0].is_error);
        assert_eq!(views[0].result_summary.as_deref(), Some("exit 1"));
    }

    #[test]
    fn running_row_expandable_for_any_finished_call_but_not_a_still_running_one() {
        let still_running =
            build_tool_call_views(&[tool_requested("a", "bash", json!({"command": "x"}))]);
        assert!(!running_row_expandable(&still_running[0]));

        let succeeded = build_tool_call_views(&[
            tool_requested("a", "bash", json!({"command": "x"})),
            tool_finished("a", json!({"exit_code": 0})),
        ]);
        assert!(running_row_expandable(&succeeded[0]));

        let failed = build_tool_call_views(&[
            tool_requested("a", "bash", json!({"command": "x"})),
            tool_finished("a", json!({"is_error": true, "message": "boom"})),
        ]);
        assert!(running_row_expandable(&failed[0]));
    }

    #[test]
    fn composer_placeholder_names_next_turn_delivery_while_a_turn_is_in_flight() {
        assert_eq!(composer_placeholder(false), "Message the agent…");
        let in_flight = composer_placeholder(true);
        assert!(in_flight.starts_with("Message the agent"));
        assert!(in_flight.contains("next turn"));
    }

    #[test]
    fn latest_turn_model_is_none_before_any_turn_completes() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
        ];
        assert_eq!(latest_turn_model(&items), None);
    }

    #[test]
    fn latest_turn_model_reads_the_most_recently_completed_turn() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("check the other form too"),
            turn_ended(TurnEndReason::Completed, Some("claude-sonnet-4"), 20),
        ];
        assert_eq!(latest_turn_model(&items), Some("claude-sonnet-4"));
    }

    #[test]
    fn latest_turn_model_skips_a_running_turns_dangling_span() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("one more thing"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
        ];
        // The second turn is still running (no closing `TurnEnded`), so its
        // model -- if any -- hasn't folded yet; the chip keeps showing the
        // last completed turn's model rather than going blank mid-turn.
        assert_eq!(latest_turn_model(&items), Some("gpt-5"));
    }

    #[test]
    fn latest_turn_model_falls_back_past_a_completed_turn_with_no_provider_request() {
        let items = vec![
            user_message("fix the bug"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 10),
            user_message("cancel immediately"),
            turn_ended(TurnEndReason::Cancelled, None, 0),
        ];
        // The most recent turn ended before any provider request (e.g. an
        // immediate cancel) and so carries no model -- the chip falls back
        // to the earlier turn's model rather than disappearing.
        assert_eq!(latest_turn_model(&items), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_shows_the_session_model_before_any_turn_completes() {
        // The gap `latest_turn_model_is_none_before_any_turn_completes`
        // exercises above: with a session-start model now known, the chip
        // no longer has to wait for the first turn to complete.
        assert_eq!(composer_model_chip(Some("gpt-5"), None), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_prefers_the_session_model_when_the_turn_model_agrees() {
        assert_eq!(
            composer_model_chip(Some("gpt-5"), Some("gpt-5")),
            Some("gpt-5")
        );
    }

    #[test]
    fn composer_model_chip_lets_a_diverging_turn_model_override_the_session_model() {
        // A future model switcher (unbuilt) could change what a session
        // actually runs mid-session -- the latest completed turn is closer
        // to "what would happen if you sent a message right now" than the
        // value resolved once at session start.
        assert_eq!(
            composer_model_chip(Some("gpt-5"), Some("claude-sonnet-4")),
            Some("claude-sonnet-4")
        );
    }

    #[test]
    fn composer_model_chip_falls_back_to_the_turn_model_when_the_session_model_is_unknown() {
        // e.g. a role-less session, or a provider with no resolvable model
        // (`contract::Provider::resolved_model`'s doc comment) -- the latest
        // completed turn is still the best available value.
        assert_eq!(composer_model_chip(None, Some("gpt-5")), Some("gpt-5"));
    }

    #[test]
    fn composer_model_chip_is_none_when_neither_is_known() {
        assert_eq!(composer_model_chip(None, None), None);
    }

    #[test]
    fn a_call_with_no_approval_request_has_approval_state_none() {
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 1})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::None);
    }

    #[test]
    fn a_call_with_an_unresolved_approval_request_is_waiting() {
        let items = vec![
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            approval_requested("a"),
            // no tool_finished yet: still pending.
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Waiting);
    }

    #[test]
    fn a_call_whose_tool_call_started_folded_is_approved_even_while_still_running() {
        // Root-caused 2026-07-13: `bash`'s approve ack folds
        // `ToolCallStarted` synchronously, one IPC hop after the click,
        // with the eventual `ToolCallFinished` arriving later and
        // asynchronously. The row must read `Approved` (buttons/proposal
        // body gone, muted "approved" phrase shown) the moment the ack
        // folds -- not stay `Waiting` for the whole tool run.
        let items = vec![
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            approval_requested("a"),
            tool_started("a"),
            // no tool_finished yet: the command is still running.
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Approved);
        assert!(!views[0].finished);
    }

    #[test]
    fn a_call_resolved_with_the_denied_marker_is_denied() {
        // The current production path: `ToolCallResult::denied` sets the
        // contract-explicit marker, read directly with no message-text
        // sniffing at all.
        let items = vec![
            tool_requested("a", "bash", json!({"command": "rm -rf /tmp/x"})),
            approval_requested("a"),
            AgentFrameItem::ToolCallFinished(ToolCallResult::denied(
                ToolCallId("a".to_string()),
                json!({"is_error": true, "message": "denied by user"}),
            )),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Denied);
    }

    #[test]
    fn a_call_resolved_with_the_denied_by_user_convention_is_denied() {
        // The fallback path: `tool_finished` builds its `ToolCallResult`
        // via `ToolCallResult::new`, which never sets `denied` -- exactly
        // what a pre-marker persisted JSONL log deserializes as
        // (`#[serde(default)]`). Classification must still land on
        // `Denied` by recognizing the old message-text convention.
        let items = vec![
            tool_requested("a", "bash", json!({"command": "rm -rf /tmp/x"})),
            approval_requested("a"),
            tool_finished("a", json!({"is_error": true, "message": "denied by user"})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Denied);
    }

    #[test]
    fn a_call_resolved_successfully_after_approval_is_approved() {
        let items = vec![
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            approval_requested("a"),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Approved);
    }

    #[test]
    fn an_approved_call_that_then_fails_on_its_own_is_still_approved_not_denied() {
        // Distinguishes a genuine denial from an *approved* call that
        // later fails for its own reasons (e.g. fs.edit's old_string not
        // found) -- both are `is_error: true`, but only the denial
        // carries the exact "denied by user" message.
        let items = vec![
            tool_requested(
                "a",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            approval_requested("a"),
            tool_finished(
                "a",
                json!({"is_error": true, "message": "`old_string` not found in `a.rs`"}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].approval, ApprovalState::Approved);
    }

    #[test]
    fn fs_edit_derives_a_diffstat_from_old_and_new_string() {
        let items = vec![
            tool_requested(
                "a",
                "fs.edit",
                json!({
                    "path": "src/agent/view.rs",
                    "old_string": "line1\nold\nline3",
                    "new_string": "line1\nnew a\nnew b\nline3",
                }),
            ),
            tool_finished("a", json!({"path": "src/agent/view.rs", "replaced": true})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Edit");
        assert_eq!(views[0].target.as_deref(), Some("src/agent/view.rs"));
        assert_eq!(views[0].result_summary.as_deref(), Some("+2 -1"));
        match &views[0].kind {
            ToolCallKind::File {
                file_name,
                diffstat,
            } => {
                assert_eq!(file_name, "view.rs");
                assert_eq!(*diffstat, Some((2, 1)));
            }
            other => panic!("expected a File chip, got {other:?}"),
        }
    }

    #[test]
    fn fs_write_reports_created_vs_overwritten_with_no_diffstat() {
        let items = vec![
            tool_requested(
                "a",
                "fs.write",
                json!({"path": "new.rs", "content": "fn main() {}"}),
            ),
            tool_finished(
                "a",
                json!({"path": "new.rs", "bytes_written": 12, "created": true}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Write");
        assert_eq!(views[0].result_summary.as_deref(), Some("created"));
        match &views[0].kind {
            ToolCallKind::File { diffstat, .. } => assert_eq!(*diffstat, None),
            other => panic!("expected a File chip, got {other:?}"),
        }
    }

    #[test]
    fn bash_chip_carries_a_truncated_command_head() {
        let long_command = "cargo test --workspace --all-targets -- --nocapture and-then-some-more";
        let items = vec![tool_requested(
            "a",
            "bash",
            json!({"command": long_command}),
        )];
        let views = build_tool_call_views(&items);
        match &views[0].kind {
            ToolCallKind::Bash { command_head } => {
                assert!(command_head.ends_with('…'));
                assert!(command_head.chars().count() <= 32);
            }
            other => panic!("expected a Bash chip, got {other:?}"),
        }
    }

    #[test]
    fn next_composer_mode_is_normal_for_an_empty_queue() {
        assert_eq!(next_composer_mode(&[], None), ComposerMode::Normal);
    }

    #[test]
    fn next_composer_mode_shows_the_oldest_actionable_call() {
        let queue = vec![ToolCallId("a".to_string()), ToolCallId("b".to_string())];
        assert_eq!(
            next_composer_mode(&queue, None),
            ComposerMode::Approval {
                call_id: ToolCallId("a".to_string())
            }
        );
    }

    #[test]
    fn next_composer_mode_stays_normal_while_the_dismissed_call_is_still_the_head() {
        // The no-flap rule: typing past the shown approval dismisses that
        // exact call_id, and it keeps reporting `Normal` for that same
        // head on every subsequent call (e.g. once per keystroke) --
        // never re-showing the approval state underneath what the user is
        // typing.
        let queue = vec![ToolCallId("a".to_string())];
        assert_eq!(
            next_composer_mode(&queue, Some(&ToolCallId("a".to_string()))),
            ComposerMode::Normal
        );
    }

    #[test]
    fn next_composer_mode_advances_once_the_dismissed_call_resolves() {
        // Decision 4's "smoothly advance": once the previously-dismissed
        // head resolves (row button/palette/CLI) and a different call
        // becomes the head, approval mode reappears for the new one --
        // the dismissal doesn't carry over to a call it was never shown
        // for.
        let queue = vec![ToolCallId("b".to_string())];
        assert_eq!(
            next_composer_mode(&queue, Some(&ToolCallId("a".to_string()))),
            ComposerMode::Approval {
                call_id: ToolCallId("b".to_string())
            }
        );
    }

    #[test]
    fn next_composer_mode_clears_once_the_queue_empties() {
        // A stale dismissal for a call that has since left the queue
        // entirely (every pending approval resolved) doesn't matter --
        // an empty queue is always `Normal`.
        assert_eq!(
            next_composer_mode(&[], Some(&ToolCallId("a".to_string()))),
            ComposerMode::Normal
        );
    }

    #[test]
    fn approving_a_bash_call_advances_composer_mode_the_instant_started_folds() {
        // End-to-end through the real seam `AgentView::sync_composer_mode`
        // uses (`horizon_agent::frame::actionable_pending_approval_call_ids_in`
        // feeding `next_composer_mode`): approving targets the oldest
        // actionable call; the daemon's synchronous ack for that click
        // folds `ToolCallStarted` immediately, well before `bash`'s
        // eventual `ToolCallFinished` -- the composer must advance to the
        // next actionable call right there, not wait for the result.
        let before = vec![approval_requested("a"), approval_requested("b")];
        let queue_before = horizon_agent::frame::actionable_pending_approval_call_ids_in(&before);
        assert_eq!(
            next_composer_mode(&queue_before, None),
            ComposerMode::Approval {
                call_id: ToolCallId("a".to_string())
            }
        );

        let after = vec![
            approval_requested("a"),
            approval_requested("b"),
            tool_started("a"),
        ];
        let queue_after = horizon_agent::frame::actionable_pending_approval_call_ids_in(&after);
        assert_eq!(
            next_composer_mode(&queue_after, None),
            ComposerMode::Approval {
                call_id: ToolCallId("b".to_string())
            }
        );
    }

    #[test]
    fn approving_the_only_pending_call_clears_composer_mode_once_started_folds() {
        let items = vec![approval_requested("a"), tool_started("a")];
        let queue = horizon_agent::frame::actionable_pending_approval_call_ids_in(&items);
        assert_eq!(next_composer_mode(&queue, None), ComposerMode::Normal);
    }

    #[test]
    fn is_keyboard_approval_target_true_only_for_the_modes_own_call() {
        let a = ToolCallId("a".to_string());
        let b = ToolCallId("b".to_string());
        let mode = ComposerMode::Approval { call_id: a.clone() };
        assert!(is_keyboard_approval_target(&mode, &a));
        assert!(!is_keyboard_approval_target(&mode, &b));
    }

    #[test]
    fn is_keyboard_approval_target_is_false_while_normal() {
        // Dismissed-by-typing (or never-pending) both collapse to
        // `Normal`, which targets no call at all -- the annotation must
        // vanish from whatever row last showed it.
        let a = ToolCallId("a".to_string());
        assert!(!is_keyboard_approval_target(&ComposerMode::Normal, &a));
    }

    #[test]
    fn progress_counts_finished_vs_total_tool_calls() {
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_requested("b", "fs.read", json!({"path": "b.rs"})),
            tool_requested("c", "fs.read", json!({"path": "c.rs"})),
            tool_finished("a", json!({"total_lines": 1})),
            tool_finished("b", json!({"total_lines": 1})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(progress(&views), (2, 3));
    }

    fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(call_id.to_string()),
            reason: "writes a file".to_string(),
        })
    }

    #[test]
    fn a_resolved_approval_within_the_turn_is_no_longer_pending() {
        let call_id = ToolCallId("a".to_string());
        let items = vec![
            approval_requested("a"),
            tool_finished("a", json!({"path": "x.rs", "replaced": true})),
        ];
        assert!(!is_approval_still_pending(&items, &call_id));
    }

    #[test]
    fn an_unresolved_approval_is_still_pending_defensively() {
        // Shouldn't happen by contract (a turn shouldn't end with a
        // dangling approval), but a `Halted`/`Cancelled` turn could leave
        // one -- the completed-turn receipt still renders it rather than
        // silently dropping it.
        let call_id = ToolCallId("a".to_string());
        let items = vec![approval_requested("a")];
        assert!(is_approval_still_pending(&items, &call_id));
    }

    fn diff_texts(lines: &[DiffLine]) -> Vec<(DiffLineKind, &str)> {
        lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect()
    }

    #[test]
    fn reconstruct_line_diff_handles_a_pure_insert() {
        let lines = reconstruct_line_diff("a\nb", "a\nnew\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Added, "new"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_pure_delete() {
        let lines = reconstruct_line_diff("a\nold\nb", "a\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_mixed_change() {
        let lines = reconstruct_line_diff("a\nold1\nold2\nb", "a\nnew1\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old1"),
                (DiffLineKind::Removed, "old2"),
                (DiffLineKind::Added, "new1"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_of_identical_strings_is_all_context() {
        let lines = reconstruct_line_diff("a\nb\nc", "a\nb\nc");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Context, "b"),
                (DiffLineKind::Context, "c"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_round_trips_multibyte_content() {
        let lines = reconstruct_line_diff(
            "こんにちは\n古い行\nさようなら",
            "こんにちは\n新しい行\nさようなら",
        );
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "こんにちは"),
                (DiffLineKind::Removed, "古い行"),
                (DiffLineKind::Added, "新しい行"),
                (DiffLineKind::Context, "さようなら"),
            ]
        );
    }

    #[test]
    fn line_diffstat_matches_the_reconstructed_diffs_own_counts() {
        assert_eq!(line_diffstat("a\nold1\nold2\nb", "a\nnew1\nb"), (1, 2));
        assert_eq!(line_diffstat("a\nb\nc", "a\nb\nc"), (0, 0));
    }

    #[test]
    fn cap_lines_head_trims_the_tail_and_reports_the_omitted_count() {
        let (kept, omitted) = cap_lines_head(vec![1, 2, 3, 4, 5], 3);
        assert_eq!(kept, vec![1, 2, 3]);
        assert_eq!(omitted, 2);

        let (kept, omitted) = cap_lines_head(vec![1, 2], 3);
        assert_eq!(kept, vec![1, 2]);
        assert_eq!(omitted, 0);
    }

    #[test]
    fn cap_lines_tail_trims_the_head_and_reports_the_omitted_count() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (kept, omitted) = cap_lines_tail(lines, 2);
        assert_eq!(kept, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(omitted, 1);
    }

    #[test]
    fn cap_thinking_text_keeps_everything_when_it_already_fits() {
        let (kept, omitted) = cap_thinking_text("one\ntwo\nthree", 6);
        assert_eq!(kept, "one\ntwo\nthree");
        assert_eq!(omitted, 0);
    }

    #[test]
    fn cap_thinking_text_keeps_only_the_trailing_lines_once_it_overflows() {
        let text = "one\ntwo\nthree\nfour\nfive";
        let (kept, omitted) = cap_thinking_text(text, 2);
        // The newest lines survive -- the earlier ones are the ones
        // dropped, matching "newest content visible" (owner requirement).
        assert_eq!(kept, "four\nfive");
        assert_eq!(omitted, 3);
    }

    #[test]
    fn cap_thinking_text_bounds_a_streaming_block_growing_delta_by_delta() {
        // The reducer coalesces every `ReasoningDelta` into one item's
        // growing `.text` (`frame.rs`'s `Event::ReasoningDelta` fold) --
        // this pins that re-running the cap on each successive render
        // never lets the *rendered* line count grow past the cap, even
        // though the underlying accumulated text keeps growing.
        let mut accumulated = String::new();
        let mut last_kept_lines = 0;
        for line in 0..20 {
            if !accumulated.is_empty() {
                accumulated.push('\n');
            }
            accumulated.push_str(&format!("thought {line}"));
            let (kept, _omitted) = cap_thinking_text(&accumulated, THINKING_TAIL_LINES);
            last_kept_lines = kept.lines().count();
            assert!(last_kept_lines <= THINKING_TAIL_LINES);
        }
        assert_eq!(last_kept_lines, THINKING_TAIL_LINES);
    }

    #[test]
    fn thinking_visible_outside_burst_only_while_the_turn_is_running() {
        assert!(thinking_visible_outside_burst(None));
        let end = TurnEnd {
            reason: TurnEndReason::Completed,
            model: None,
            elapsed: Duration::ZERO,
        };
        assert!(!thinking_visible_outside_burst(Some(&end)));
    }

    #[test]
    fn segment_bursts_never_lets_a_stray_reasoning_delta_split_a_burst() {
        // `segment_bursts`'s own doc comment calls out "a stray reasoning
        // delta" between two tool-related items of the same burst as
        // absorbed, not boundary-affecting -- pin it directly so the
        // burst-absorption half of this fix's design fork (thinking
        // structurally inside a burst's range stays invisible, unchanged)
        // has its own regression coverage.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            reasoning_delta("considering the second call…"),
            tool_requested("b", "fs.read", json!({"path": "b.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            tool_finished("b", json!({"total_lines": 5})),
            assistant_delta("Looking at both files, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].start, 1);
        assert_eq!(bursts[0].end, 6);
        assert!(bursts[0].closed);
    }

    #[test]
    fn build_tool_call_body_reconstructs_an_fs_edit_diff() {
        let body = build_tool_call_body(
            "fs.edit",
            &json!({
                "path": "src/agent/view.rs",
                "old_string": "line1\nold\nline3",
                "new_string": "line1\nnew a\nnew b\nline3",
            }),
            Some(&json!({"path": "src/agent/view.rs", "replaced": true})),
        );
        match body {
            ToolCallBody::Diff { lines, omitted } => {
                assert_eq!(omitted, 0);
                assert_eq!(
                    diff_texts(&lines),
                    vec![
                        (DiffLineKind::Context, "line1"),
                        (DiffLineKind::Removed, "old"),
                        (DiffLineKind::Added, "new a"),
                        (DiffLineKind::Added, "new b"),
                        (DiffLineKind::Context, "line3"),
                    ]
                );
            }
            other => panic!("expected a Diff body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_labels_fs_write_created_vs_overwritten() {
        let created = build_tool_call_body(
            "fs.write",
            &json!({"path": "new.rs", "content": "fn main() {}"}),
            Some(&json!({"path": "new.rs", "bytes_written": 12, "created": true})),
        );
        match created {
            ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            } => {
                assert_eq!(label, "created");
                assert_eq!(lines, vec!["fn main() {}".to_string()]);
                assert_eq!(omitted, 0);
            }
            other => panic!("expected a ContentPreview body, got {other:?}"),
        }

        let overwritten = build_tool_call_body(
            "fs.write",
            &json!({"path": "old.rs", "content": "x"}),
            Some(&json!({"path": "old.rs", "bytes_written": 1, "created": false})),
        );
        match overwritten {
            ToolCallBody::ContentPreview { label, .. } => assert_eq!(label, "overwritten"),
            other => panic!("expected a ContentPreview body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_carries_bash_command_exit_code_and_output() {
        let body = build_tool_call_body(
            "bash",
            &json!({"command": "cargo test"}),
            Some(&json!({"exit_code": 0, "output": "line1\nline2\n", "truncated": false})),
        );
        match body {
            ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            } => {
                assert_eq!(command, "cargo test");
                assert_eq!(exit_code, Some(0));
                assert_eq!(lines, vec!["line1".to_string(), "line2".to_string()]);
                assert_eq!(omitted, 0);
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_tail_caps_a_long_bash_output() {
        let output_text = (0..(BASH_OUTPUT_TAIL_LINES + 10))
            .map(|line_number| format!("line {line_number}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = build_tool_call_body(
            "bash",
            &json!({"command": "seq"}),
            Some(&json!({"exit_code": 0, "output": output_text})),
        );
        match body {
            ToolCallBody::Command { lines, omitted, .. } => {
                assert_eq!(omitted, 10);
                assert_eq!(lines.len(), BASH_OUTPUT_TAIL_LINES);
                // The tail is kept, not the head.
                assert_eq!(lines.last().unwrap(), "line 109");
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_read_with_the_line_range() {
        let body = build_tool_call_body(
            "fs.read",
            &json!({"path": "src/lib.rs"}),
            Some(&json!({"start_line": 1, "end_line": 40, "total_lines": 120})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("src/lib.rs · lines 1-40 of 120".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_grep_with_the_match_count() {
        let body = build_tool_call_body(
            "fs.grep",
            &json!({"base_path": ".", "pattern": "notify"}),
            Some(&json!({"returned_count": 3})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("\"notify\" in . · 3 matches".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_glob_with_the_match_count() {
        let body = build_tool_call_body(
            "fs.glob",
            &json!({"base_path": ".", "pattern": "*.rs"}),
            Some(&json!({"returned_count": 5})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("*.rs in . · 5 matches".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_falls_back_to_raw_json_for_an_unknown_tool() {
        let body = build_tool_call_body(
            "some.future.tool",
            &json!({"foo": "bar"}),
            Some(&json!({"ok": true})),
        );
        match body {
            ToolCallBody::Raw { lines, omitted } => {
                assert_eq!(omitted, 0);
                let joined = lines.join("\n");
                assert!(joined.contains("some.future.tool"));
                assert!(joined.contains("\"foo\""));
                assert!(joined.contains("\"ok\""));
            }
            other => panic!("expected a Raw body, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_body_finds_the_matching_call_within_a_turns_items() {
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("a", json!({"total_lines": 10})),
            tool_finished("b", json!({"path": "b.rs", "replaced": true})),
        ];
        let call_id = ToolCallId("b".to_string());
        match tool_call_body(&items, &call_id) {
            Some(ToolCallBody::Diff { lines, .. }) => {
                assert_eq!(
                    diff_texts(&lines),
                    vec![(DiffLineKind::Removed, "x"), (DiffLineKind::Added, "y")]
                );
            }
            other => panic!("expected a Diff body for call `b`, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_body_is_none_for_an_unknown_call_id() {
        let items = vec![tool_requested("a", "fs.read", json!({"path": "a.rs"}))];
        let call_id = ToolCallId("missing".to_string());
        assert!(tool_call_body(&items, &call_id).is_none());
    }

    #[test]
    fn tool_call_body_for_a_waiting_bash_call_carries_the_full_command_not_the_row_head() {
        // Row-centric approval v2: a `Waiting` row auto-displays this body
        // as its proposal (decision 4's "proposal — not applied") before
        // any `ToolCallFinished` exists -- unlike `ToolCallKind::Bash`'s
        // `command_head` (the row's own collapsed line and the receipt
        // chip), which truncates to the first line's first 32 characters
        // (see `bash_chip_carries_a_truncated_command_head`).
        let long_command = format!("echo {}", "x".repeat(50));
        let items = vec![
            tool_requested("a", "bash", json!({"command": long_command})),
            approval_requested("a"),
        ];
        match tool_call_body(&items, &ToolCallId("a".to_string())) {
            Some(ToolCallBody::Command {
                command, exit_code, ..
            }) => {
                assert_eq!(command, format!("echo {}", "x".repeat(50)));
                assert!(command.chars().count() > 32);
                assert_eq!(exit_code, None);
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }

    #[test]
    fn classify_call_sorts_every_tool_id_into_its_class() {
        assert_eq!(classify_call("fs.edit"), CallClass::Edit);
        assert_eq!(classify_call("fs.write"), CallClass::Edit);
        assert_eq!(classify_call("bash"), CallClass::Bash);
        for tool_id in [
            "fs.read",
            "fs.grep",
            "fs.glob",
            "recall.search",
            "recall.read",
            "workspace.snapshot",
            "skill.read",
            "some.future.tool",
        ] {
            assert_eq!(classify_call(tool_id), CallClass::Query, "{tool_id}");
        }
    }

    #[test]
    fn aggregate_receipt_folds_mixed_classes_into_prose_counts() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
            tool_requested(
                "q2",
                "fs.glob",
                json!({"base_path": ".", "pattern": "*.rs"}),
            ),
            tool_finished("q2", json!({"returned_count": 2})),
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "b.rs", "replaced": true})),
            tool_requested("b1", "bash", json!({"command": "cargo test"})),
            tool_finished("b1", json!({"exit_code": 0, "output": ""})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.query_count, 2); // fs.grep + fs.glob
        assert_eq!(aggregate.read_file_count, 1);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(aggregate.bash_count, 1);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("2 tool calls · read 1 file · edited 1 file · ran 1 command")
        );
    }

    #[test]
    fn aggregate_receipt_counts_distinct_paths_not_call_counts() {
        let items = vec![
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested("r2", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r2", json!({"total_lines": 10})),
            tool_requested("r3", "fs.read", json!({"path": "b.rs"})),
            tool_finished("r3", json!({"total_lines": 5})),
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "c.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "c.rs", "replaced": true})),
            tool_requested("e2", "fs.write", json!({"path": "c.rs", "content": "z"})),
            tool_finished("e2", json!({"path": "c.rs", "created": false})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        // Two reads of a.rs collapse to one distinct path; b.rs adds a
        // second. An edit and a write to the same c.rs collapse to one
        // distinct edited path.
        assert_eq!(aggregate.read_file_count, 2);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("read 2 files · edited 1 file")
        );
    }

    #[test]
    fn aggregate_receipt_breaks_out_a_failed_call_of_any_class_individually() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
            tool_requested("bad_read", "fs.read", json!({"path": "missing.rs"})),
            tool_finished(
                "bad_read",
                json!({"is_error": true, "message": "not found"}),
            ),
            tool_requested(
                "bad_edit",
                "fs.edit",
                json!({"path": "d.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished(
                "bad_edit",
                json!({"is_error": true, "message": "old_string not found"}),
            ),
            tool_requested("bad_bash", "bash", json!({"command": "false"})),
            tool_finished(
                "bad_bash",
                json!({"is_error": true, "message": "boom", "exit_code": 1}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        // The failed read, edit, and bash never reach any count...
        assert_eq!(aggregate.read_file_count, 0);
        assert_eq!(aggregate.edited_file_count, 0);
        assert_eq!(aggregate.bash_count, 0);
        assert_eq!(aggregate.query_count, 1); // only the successful grep
                                              // ...and stay individually chip-able instead, regardless of class.
        assert_eq!(aggregate.individual_calls.len(), 3);
        let individual_ids: Vec<&str> = aggregate
            .individual_calls
            .iter()
            .map(|call| call.call_id.0.as_str())
            .collect();
        assert!(individual_ids.contains(&"bad_read"));
        assert!(individual_ids.contains(&"bad_edit"));
        assert!(individual_ids.contains(&"bad_bash"));
    }

    #[test]
    fn receipt_prose_uses_singular_wording_for_a_count_of_one() {
        let aggregate = ReceiptAggregate {
            query_count: 1,
            read_file_count: 1,
            edited_file_count: 1,
            bash_count: 1,
            ..Default::default()
        };
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file · edited 1 file · ran 1 command")
        );
    }

    #[test]
    fn receipt_prose_uses_plural_wording_above_one() {
        let aggregate = ReceiptAggregate {
            query_count: 3,
            read_file_count: 2,
            edited_file_count: 5,
            bash_count: 4,
            ..Default::default()
        };
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("3 tool calls · read 2 files · edited 5 files · ran 4 commands")
        );
    }

    #[test]
    fn receipt_prose_is_none_when_every_count_is_zero() {
        // An all-individual-chip turn (every call failed, or is the
        // defensive never-finished case): the collapsed line still
        // shows those chips plus status/elapsed (view concern), but the
        // prose prefix itself is simply absent.
        assert_eq!(receipt_prose(&ReceiptAggregate::default()), None);
    }

    #[test]
    fn aggregate_changes_is_empty_when_no_file_was_ever_touched() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
        assert_eq!(changes_summary_text(&aggregate_changes(&tool_calls)), None);
    }

    #[test]
    fn aggregate_changes_sums_diffstats_across_multiple_edits_to_one_file() {
        // Decision 9's documented "summed hunk stats, not a net diff"
        // limitation: two edits to the same file each contribute their
        // own reconstructed diffstat, added together.
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "src/a.rs", "old_string": "x", "new_string": "y\nz"}),
            ),
            tool_finished("e1", json!({"path": "src/a.rs", "replaced": true})),
            tool_requested(
                "e2",
                "fs.edit",
                json!({"path": "src/a.rs", "old_string": "y\nz", "new_string": "w"}),
            ),
            tool_finished("e2", json!({"path": "src/a.rs", "replaced": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].file_name, "a.rs");
        // e1: +2 -1 (y\nz replaces x); e2: +1 -2 (w replaces y\nz).
        assert_eq!(changes[0].added, 3);
        assert_eq!(changes[0].removed, 3);
        assert!(!changes[0].created);
    }

    #[test]
    fn aggregate_changes_orders_by_first_touch() {
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "b.rs", "replaced": true})),
            tool_requested(
                "e2",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e2", json!({"path": "a.rs", "replaced": true})),
            // A second touch of b.rs must not move it later in the order.
            tool_requested(
                "e3",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "y", "new_string": "z"}),
            ),
            tool_finished("e3", json!({"path": "b.rs", "replaced": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs"]);
    }

    #[test]
    fn aggregate_changes_flags_fs_write_created() {
        let items = vec![
            tool_requested(
                "w1",
                "fs.write",
                json!({"path": "new.rs", "content": "fn main() {}"}),
            ),
            tool_finished("w1", json!({"path": "new.rs", "created": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].created);
        // fs.write never produces a diffstat.
        assert_eq!(changes[0].added, 0);
        assert_eq!(changes[0].removed, 0);
    }

    #[test]
    fn aggregate_changes_does_not_flag_an_overwrite_as_created() {
        let items = vec![
            tool_requested(
                "w1",
                "fs.write",
                json!({"path": "existing.rs", "content": "fn main() {}"}),
            ),
            tool_finished("w1", json!({"path": "existing.rs", "created": false})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].created);
    }

    #[test]
    fn aggregate_changes_excludes_a_failed_edit() {
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished(
                "e1",
                json!({"is_error": true, "message": "old_string not found"}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
    }

    #[test]
    fn aggregate_changes_ignores_non_edit_calls() {
        let items = vec![
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested("b1", "bash", json!({"command": "cargo test"})),
            tool_finished("b1", json!({"exit_code": 0})),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
    }

    #[test]
    fn changes_summary_text_formats_files_and_totals() {
        let changes = vec![
            FileChange {
                path: "a.rs".to_string(),
                file_name: "a.rs".to_string(),
                added: 100,
                removed: 20,
                created: false,
            },
            FileChange {
                path: "b.rs".to_string(),
                file_name: "b.rs".to_string(),
                added: 20,
                removed: 16,
                created: true,
            },
        ];
        assert_eq!(
            changes_summary_text(&changes).as_deref(),
            Some("2 files · +120 −36")
        );
    }

    #[test]
    fn changes_summary_text_uses_singular_wording_for_one_file() {
        let changes = vec![FileChange {
            path: "a.rs".to_string(),
            file_name: "a.rs".to_string(),
            added: 2,
            removed: 1,
            created: false,
        }];
        assert_eq!(
            changes_summary_text(&changes).as_deref(),
            Some("1 file · +2 −1")
        );
    }

    fn todo_write_requested(call_id: &str, items: Value) -> AgentFrameItem {
        tool_requested(call_id, "todo.write", json!({ "items": items }))
    }

    #[test]
    fn latest_todo_list_is_none_when_no_write_ever_landed() {
        let items = vec![
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
        ];
        assert!(latest_todo_list(&items).is_none());
        assert_eq!(todo_summary_text(&[]), None);
    }

    #[test]
    fn latest_todo_list_reads_the_most_recent_successful_write() {
        let items = vec![
            todo_write_requested("t1", json!([{"text": "step one", "status": "pending"}])),
            tool_finished("t1", json!({"total": 1, "pending": 1})),
            todo_write_requested(
                "t2",
                json!([
                    {"text": "step one", "status": "done"},
                    {"text": "step two", "status": "in_progress"},
                ]),
            ),
            tool_finished("t2", json!({"total": 2, "done": 1, "in_progress": 1})),
        ];
        let list = latest_todo_list(&items).expect("a successful write landed");
        assert_eq!(
            list,
            vec![
                TodoItem {
                    text: "step one".to_string(),
                    status: TodoStatus::Done,
                },
                TodoItem {
                    text: "step two".to_string(),
                    status: TodoStatus::InProgress,
                },
            ]
        );
        assert_eq!(todo_summary_text(&list).as_deref(), Some("1/2 done"));
    }

    #[test]
    fn latest_todo_list_ignores_a_failed_write() {
        let items = vec![
            todo_write_requested("t1", json!([{"text": "step one", "status": "done"}])),
            tool_finished("t1", json!({"total": 1, "done": 1})),
            todo_write_requested("t2", json!([{"text": "bad", "status": "blocked"}])),
            tool_finished(
                "t2",
                json!({"is_error": true, "message": "item 0 has an invalid `status`"}),
            ),
        ];
        let list = latest_todo_list(&items).expect("t1's write is still the latest success");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].status, TodoStatus::Done);
    }

    #[test]
    fn latest_todo_list_ignores_a_still_in_flight_write() {
        let items = vec![todo_write_requested(
            "t1",
            json!([{"text": "step one", "status": "pending"}]),
        )];
        assert!(latest_todo_list(&items).is_none());
    }

    #[test]
    fn latest_todo_list_folds_an_explicit_clear_to_an_empty_list_and_hides_the_panel() {
        let items = vec![
            todo_write_requested("t1", json!([{"text": "step one", "status": "done"}])),
            tool_finished("t1", json!({"total": 1, "done": 1})),
            todo_write_requested("t2", json!([])),
            tool_finished("t2", json!({"total": 0})),
        ];
        let list = latest_todo_list(&items).expect("t2's write is a successful, empty list");
        assert!(list.is_empty());
        assert_eq!(todo_summary_text(&list), None);
    }

    #[test]
    fn aggregate_receipt_folds_bash_into_the_ran_commands_count() {
        // Owner feedback 2026-07-13 (round 3 follow-up): a dozen
        // near-identical bash chips (e.g. every command sharing the same
        // `cd … && …` prefix) conveyed nothing -- bash now aggregates
        // into prose exactly like query/edit calls, leaving no chip
        // behind for a successful run.
        let items = vec![
            tool_requested("b1", "bash", json!({"command": "cargo build"})),
            tool_finished("b1", json!({"exit_code": 0, "output": ""})),
            tool_requested("b2", "bash", json!({"command": "cargo test"})),
            tool_finished("b2", json!({"exit_code": 0, "output": ""})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.bash_count, 2);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(receipt_prose(&aggregate).as_deref(), Some("ran 2 commands"));
    }

    #[test]
    fn segment_bursts_is_empty_for_an_all_prose_turn() {
        // Nothing worth a receipt for -- the text keeps rendering as
        // plain prose, exactly as it always has.
        let items = vec![user_message("hi"), assistant_delta("hello there")];
        assert_eq!(segment_bursts(&items), Vec::new());
    }

    #[test]
    fn segment_bursts_finds_a_single_open_burst_while_tools_are_unfinished() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            // no matching tool_finished("a", ..) yet
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 2,
                closed: false,
            }]
        );
    }

    #[test]
    fn segment_bursts_stays_open_while_an_approval_is_pending() {
        // A pending approval means its call has no `ToolCallFinished`
        // yet -- covered by the same "every call finished" check, no
        // separate approval-specific branch needed.
        let items = vec![
            user_message("delete the file"),
            approval_requested("a"),
            // no matching tool_finished("a", ..) yet: still pending.
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert!(!bursts[0].closed);
    }

    #[test]
    fn segment_bursts_closes_once_tools_are_done_and_text_follows() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_starts_a_new_burst_for_a_tool_call_after_closing_text() {
        // The model answered, then decided to run one more tool call --
        // round 5 (monotone splitting): the first burst stays closed
        // forever, and this is a brand new *second* burst, not a reopen.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![
                Burst {
                    start: 1,
                    end: 3,
                    closed: true,
                },
                Burst {
                    start: 4,
                    end: 5,
                    closed: false,
                },
            ]
        );
    }

    #[test]
    fn segment_bursts_closes_on_a_committed_assistant_message_too() {
        // Accepts either a streaming delta or an already-committed
        // assistant `Message` as the closing text.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": "ok"})),
            assistant_message("Fixed it, tests pass."),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        assert!(bursts[0].closed);
    }

    #[test]
    fn segment_bursts_an_interjected_user_message_never_closes_a_burst() {
        // The user typing again mid-burst doesn't count as assistant
        // text and doesn't split anything -- a later tool call still
        // just extends the same still-open burst, which then closes
        // normally once real (assistant) text follows it.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo build"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            user_message("still there?"),
            tool_requested("b", "bash", json!({"command": "cargo test"})),
            tool_finished("b", json!({"exit_code": 0, "output": ""})),
        ];
        // No assistant text anywhere yet: one still-open burst spanning
        // straight through the interjection to both bash calls.
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 6,
                closed: false,
            }]
        );

        let mut closed_items = items;
        closed_items.push(assistant_message("Both ran fine."));
        assert_eq!(
            segment_bursts(&closed_items),
            vec![Burst {
                start: 1,
                end: 6,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_two_tool_text_tool_runs_are_two_bursts() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Found it, fixing now."),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("b", json!({"path": "a.rs", "replaced": true})),
            assistant_message("Fixed."),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![
                Burst {
                    start: 1,
                    end: 3,
                    closed: true,
                },
                Burst {
                    start: 4,
                    end: 6,
                    closed: true,
                },
            ]
        );
    }

    #[test]
    fn segment_bursts_turn_ended_closes_the_trailing_burst_even_with_no_closing_text() {
        // Tools ran right up to the end -- no assistant text ever
        // followed them, but `TurnEnded` still closes the burst (it
        // folds directly into the final receipt, `AgentView::
        // render_turn`'s job, not this function's).
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 12),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn segment_bursts_turn_ended_closes_an_already_text_closed_burst_the_same_way() {
        // The common case: tools finish, text follows (closes the
        // burst already), then `TurnEnded` arrives -- still exactly one
        // closed burst, unaffected by the extra close signal.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": ""})),
            assistant_message("Done, tests pass."),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 12),
        ];
        assert_eq!(
            segment_bursts(&items),
            vec![Burst {
                start: 1,
                end: 3,
                closed: true,
            }]
        );
    }

    #[test]
    fn a_burst_reconstructs_the_same_receipt_content_a_completed_turns_own_aggregation_would() {
        // A closed burst's own item range feeds `aggregate_receipt`/
        // `receipt_prose` exactly the way a whole completed turn's items
        // used to -- proving per-burst aggregation reuses the existing
        // machinery verbatim, just scoped to the burst's own range.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("a", json!({"returned_count": 2})),
            tool_requested("b", "fs.read", json!({"path": "a.rs"})),
            tool_finished("b", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        let burst = &bursts[0];
        assert!(burst.closed);
        let tool_calls = build_tool_call_views(&items[burst.start..burst.end]);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file")
        );
        assert_eq!(aggregate.bash_count, 0);
        assert!(aggregate.individual_calls.is_empty());
    }

    #[test]
    fn a_bursts_start_index_stays_stable_as_more_items_stream_in() {
        // Proves the rendering-side receipt key (`base_index +
        // burst.start`) stays stable across re-renders: appending more
        // items to the tail (new deltas/tool calls arriving) never
        // changes the `start` a burst already claimed in an earlier,
        // shorter snapshot of the same items.
        let short = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        let first_start = segment_bursts(&short)[0].start;

        let mut grown = short.clone();
        grown.push(assistant_delta(" think the bug is here."));
        grown.push(tool_requested(
            "b",
            "fs.edit",
            json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
        ));
        let grown_bursts = segment_bursts(&grown);
        assert_eq!(grown_bursts.len(), 2);
        assert_eq!(grown_bursts[0].start, first_start);
    }

    #[test]
    fn receipt_tail_final_carries_the_turn_end_while_intermediate_carries_nothing() {
        // Pins the two `ReceiptTail` variants' own shapes: `Final` wraps
        // a `&TurnEnd` (status/elapsed/model all recoverable from it via
        // `receipt_status`/its own fields), `Intermediate` is a unit
        // variant with nothing to recover at all -- the render side is
        // the one place status/elapsed/model ever get read from `tail`,
        // but this pins that `Intermediate` truly carries none of them
        // before that render-side code ever runs.
        let end = TurnEnd {
            reason: TurnEndReason::Completed,
            model: Some("gpt-5".to_string()),
            elapsed: Duration::from_secs(38),
        };
        match ReceiptTail::Final(&end) {
            ReceiptTail::Final(end) => {
                assert_eq!(receipt_status(end).text, "38s");
                assert_eq!(end.model.as_deref(), Some("gpt-5"));
            }
            ReceiptTail::Intermediate => panic!("expected Final"),
        }
        assert!(matches!(
            ReceiptTail::Intermediate,
            ReceiptTail::Intermediate
        ));
    }

    #[test]
    fn contains_user_message_finds_a_user_message_among_other_items() {
        let items = vec![
            assistant_message("hi"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            user_message("fix the bug"),
        ];
        assert!(contains_user_message(&items));
    }

    #[test]
    fn contains_user_message_false_without_one() {
        let items = vec![
            assistant_message("hi"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
        ];
        assert!(!contains_user_message(&items));
    }
}
