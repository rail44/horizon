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

/// Groups `items` into turn segments: a segment opens at a user
/// `Message` seen while no segment is currently open, and closes at the
/// next `TurnEnded` (inclusive). A trailing segment with no closing
/// `TurnEnded` yet is the turn in progress.
///
/// **Invariant (root-caused 2026-07-13 from a real, reproduced event
/// sequence -- see `docs/agent-output-ui-amendment.md`'s post-review
/// note): every span partitions the item list.** The only items that
/// legitimately fall *outside* every span are ones before the first user
/// message (a genuinely resumed history that doesn't open with one).
/// This used to not hold: a user `Message` unconditionally opened a
/// *new* segment even while the previous one hadn't seen a `TurnEnded`
/// yet, closing the stale one with `ended: None` on the theory that this
/// "shouldn't happen by contract". It does happen, routinely -- sending
/// from the composer is deliberately next-turn delivery even while a
/// turn is mid-flight (`docs/agent-output-ui-amendment.md` decision 6),
/// and a user *will* type another message while an earlier tool call's
/// approval is still pending (e.g. nudging a turn that looks stuck, or
/// retrying an approval that didn't seem to take effect). Each such
/// interjection used to mint a fresh span and stale the previous one
/// forever (it can never retroactively gain a `TurnEnded`) -- and once
/// the session's state eventually left the in-flight set (a cancel, or
/// the turn finally settling), the view's fallback for a dangling
/// `ended: None` span while not in flight is to render every item in it
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
pub(crate) fn group_into_turns(items: &[AgentFrameItem]) -> Vec<TurnSpan> {
    let mut spans = Vec::new();
    let mut current_start: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if is_user_message(item) && current_start.is_none() {
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

fn is_user_message(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            ..
        })
    )
}

/// Whether `item` is part of a tool call's lifecycle -- used by
/// [`running_turn_folds`] to find the last such item in a running turn.
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
/// committed assistant `Message` -- used by [`running_turn_folds`].
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

/// The frame-relative index (within `items`) of the last tool-related
/// item, if any.
fn last_tool_related_index(items: &[AgentFrameItem]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_tool_related(item))
        .map(|(index, _)| index)
        .max()
}

/// Whether a running turn's tool activity is done and its final response
/// has started rendering -- owner feedback 2026-07-13 ("can the fold
/// into a receipt happen when the final response STARTS rendering,
/// rather than after it finishes?"). `items` is the turn's own slice
/// (from `group_into_turns`, `ended: None`). True when:
///
/// 1. the turn has made at least one tool call (an all-prose turn has
///    nothing worth a receipt for -- it keeps rendering as plain text,
///    same as today);
/// 2. every tool call has finished (this alone also covers "no pending
///    approval": an unresolved approval means its call has no
///    `ToolCallFinished` yet, so `build_tool_call_views` reports it
///    unfinished -- the approval box, and the running card behind it,
///    keep showing exactly as before);
/// 3. assistant text (a streaming delta or a committed assistant
///    message) appears *after* the last tool-related item.
///
/// While true, the view (`AgentView::render_turn`) renders a
/// provisional receipt line -- the same aggregated prose/chips as a
/// final receipt, just with a live ticking elapsed and no end status/
/// model yet -- in place of the running card; the streaming text itself
/// keeps rendering the same way it always has, right below.
///
/// This is deliberately re-evaluated on every render rather than latched
/// once true: if the model starts answering and then makes *another*
/// tool call, condition 3 stops holding (the last tool-related item is
/// now after the trailing text that used to satisfy it) and this flips
/// back to `false` on the very next render -- the running card
/// reappears beneath whatever text had already streamed. That reversal
/// is intended, not a glitch: the turn genuinely isn't "just wrapping
/// up" anymore, so the affordances a still-working turn needs (the stop
/// button, per-row progress) belong back on screen.
pub(crate) fn running_turn_folds(items: &[AgentFrameItem]) -> bool {
    let tool_calls = build_tool_call_views(items);
    if tool_calls.is_empty() {
        return false;
    }
    if !tool_calls.iter().all(|call| call.finished) {
        return false;
    }
    match last_tool_related_index(items) {
        Some(last_tool_index) => items[last_tool_index + 1..].iter().any(is_assistant_text),
        None => false,
    }
}

/// The receipt row's trailing content (`render_receipt`'s `tail`
/// parameter): a finished turn's end-reason status + model id, or -- the
/// early-fold provisional receipt ([`running_turn_folds`], owner
/// feedback 2026-07-13) -- a live ticking elapsed with neither yet.
/// Carries a `&TurnEnd` rather than duplicating its fields so `Final`
/// can never drift from [`receipt_status`]'s own reading of it.
pub(crate) enum ReceiptTail<'a> {
    Final(&'a TurnEnd),
    Provisional { elapsed: Duration },
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
/// how its eventual `ToolCallFinished` reads (see
/// [`is_denied_output`] for the denial convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalState {
    /// No `ApprovalRequested` item for this call at all -- an
    /// auto-approved (or never-requiring-approval) tool.
    None,
    /// An `ApprovalRequested` item exists and no `ToolCallFinished` has
    /// resolved it yet.
    Waiting,
    Approved,
    Denied,
}

/// The exact denial convention `tools::approval::denied_output` writes
/// for a Horizon-executed tool's deny path:
/// `json!({ "is_error": true, "message": "denied by user" })`
/// (`crates/horizon-agent/src/tools/approval.rs`). Checked by the
/// message text specifically, not just `is_error`, because an
/// *approved* call that goes on to fail for its own reasons (e.g.
/// fs.edit's "old_string not found") is also `is_error: true` but
/// carries a different message -- `is_error` alone can't tell a denial
/// from an execution failure.
fn is_denied_output(output: &Value) -> bool {
    output.get("is_error").and_then(Value::as_bool) == Some(true)
        && output.get("message").and_then(Value::as_str) == Some("denied by user")
}

/// Derives a call's [`ApprovalState`] from whether it ever had an
/// `ApprovalRequested` item and, if resolved, its result's output.
fn derive_approval_state(
    had_approval_request: bool,
    result: Option<&ToolCallResult>,
) -> ApprovalState {
    if !had_approval_request {
        return ApprovalState::None;
    }
    match result {
        None => ApprovalState::Waiting,
        Some(result) if is_denied_output(&result.output) => ApprovalState::Denied,
        Some(_) => ApprovalState::Approved,
    }
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
                approval: derive_approval_state(entry.had_approval_request, entry.result),
            }
        })
        .collect()
}

/// Whether `call_id`'s approval request is still unresolved within
/// `turn_items` -- a single turn's own item slice is enough to answer
/// this without consulting the whole frame: every tool call this crate
/// emits, Horizon-executed or provider-forwarded, resolves via a
/// `ToolCallFinished` with the same `call_id` (see
/// `crates/horizon-agent/src/tools/approval.rs`'s `synchronous_result`,
/// the one path every approve/deny decision funnels through) before its
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
    fn a_call_resolved_with_the_denied_by_user_convention_is_denied() {
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
    fn running_turn_folds_is_false_with_no_tool_calls() {
        // An all-prose turn: nothing worth a receipt for -- the text
        // keeps rendering as it does today, no early fold.
        let items = vec![user_message("hi"), assistant_delta("hello there")];
        assert!(!running_turn_folds(&items));
    }

    #[test]
    fn running_turn_folds_is_false_while_a_tool_call_is_unfinished() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            // no matching tool_finished("a", ..) yet
        ];
        assert!(!running_turn_folds(&items));
    }

    #[test]
    fn running_turn_folds_is_false_while_an_approval_is_pending() {
        // A pending approval means its call has no `ToolCallFinished`
        // yet, so it's covered by the same "every call finished" check
        // -- no separate approval-specific branch needed.
        let items = vec![
            user_message("delete the file"),
            approval_requested("a"),
            // no matching tool_finished("a", ..) yet: still pending.
        ];
        assert!(!running_turn_folds(&items));
    }

    #[test]
    fn running_turn_folds_is_true_once_tools_are_done_and_text_follows() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_finished("a", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        assert!(running_turn_folds(&items));
    }

    #[test]
    fn running_turn_folds_flips_back_to_false_when_another_tool_call_follows_text() {
        // The model started answering, then decided to run one more
        // tool call -- the turn isn't "just wrapping up" anymore, so the
        // predicate must flip back (documented as intended, not a
        // glitch, on `running_turn_folds` itself).
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
        assert!(!running_turn_folds(&items));
    }

    #[test]
    fn running_turn_folds_is_true_with_a_committed_assistant_message_too() {
        // The predicate accepts either a streaming delta or an already-
        // committed assistant `Message` as "the final response started".
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished("a", json!({"exit_code": 0, "output": "ok"})),
            assistant_message("Fixed it, tests pass."),
        ];
        assert!(running_turn_folds(&items));
    }

    #[test]
    fn provisional_receipt_content_matches_the_final_receipts_own_aggregation() {
        // The provisional line reuses `aggregate_receipt`/`receipt_prose`
        // verbatim on the running turn's own items -- proving that reuse
        // produces the same content a final receipt would, once the
        // turn actually ends the same way.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("a", json!({"returned_count": 2})),
            tool_requested("b", "fs.read", json!({"path": "a.rs"})),
            tool_finished("b", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        assert!(running_turn_folds(&items));
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file")
        );
        assert_eq!(aggregate.bash_count, 0);
        assert!(aggregate.individual_calls.is_empty());
    }
}
