//! One tool call's view-model: classification into a display verb/target/
//! summary and approval-lifecycle derivation. The expanded per-tool body
//! (diff/content-preview/command/summary/raw-JSON) stayed in the `horizon`
//! binary crate's `src/agent/turns` -- its fallback for a terse,
//! known-but-not-specially-bodied tool leans on a wording function
//! (`terse_summary`), so the whole `ToolCallBody` family stayed with it
//! rather than splitting one function across the crate boundary (see
//! `transcript`'s module doc). The generic line-capping mechanics that
//! body construction (and the reasoning-delta cap) both lean on stayed
//! here regardless, since they're wording-free.

use crate::contract::{ToolCallId, ToolCallResult};
use crate::frame::{pending_approval_call_ids_in, AgentFrameItem};
use serde_json::Value;

use super::file_name;

/// Structured, tool-specific data a receipt chip or running-card row
/// needs beyond the generic verb/target/summary -- the file-chip
/// diffstat and the bash chip's command head (decision 1's chip
/// composition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallKind {
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
pub struct ToolCallView {
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
pub enum ApprovalState {
    /// No `ApprovalRequested` item for this call at all -- an
    /// auto-approved (or never-requiring-approval) tool.
    None,
    /// An `ApprovalRequested` item exists and neither a `ToolCallStarted`
    /// nor a `ToolCallFinished` has resolved it yet -- the row still shows
    /// Approve/Deny.
    Waiting,
    /// The user approved: a `ToolCallStarted` (immediate for `bash`,
    /// alongside `ToolCallFinished` for the synchronous fs/config tools --
    /// see `crate::tools::approval::resolve_approval`'s doc comment) has
    /// folded, whether or not the call has gone on to finish yet. The
    /// daemon acks the decision one IPC hop after the click, well before a
    /// `bash` call's result -- root-caused 2026-07-13 (owner report:
    /// buttons and the proposal body lingered for the whole tool run after
    /// the click). Buttons/proposal body disappear here; the row's glyph
    /// stays ● running until `ToolCallFinished` also folds.
    Approved,
    /// The user denied: `ToolCallFinished` folded with the "denied by
    /// user" convention, with no `ToolCallStarted` at all (a deny never
    /// starts the tool).
    Denied,
}

/// Whether `result` represents the user's tool-call denial. Reads the
/// contract-explicit [`ToolCallResult::denied`] marker first -- set at the
/// source by `tools::approval::synchronous_result`'s `ran = false` path
/// (`crate::tools::approval`) -- and falls back to [`is_denied_output`]'s
/// old message-text convention only when the marker reads `false`. That
/// fallback exists for exactly one case: a `ToolCallResult` persisted (as
/// JSONL) before the marker field existed deserializes with `denied: false`
/// regardless of its real outcome (`#[serde(default)]`), so replaying an
/// old log still needs the message text to classify those rows correctly.
/// A freshly folded denial always carries the marker and never needs the
/// fallback.
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

/// Builds one [`ToolCallView`] per distinct tool call requested within
/// `items` (a single turn span's slice), in first-request order. A call
/// with no matching `ToolCallFinished` yet (the running turn's
/// in-flight calls) gets `finished: false` and no result summary.
pub fn build_tool_call_views(items: &[AgentFrameItem]) -> Vec<ToolCallView> {
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
                // `.rev()`: attribute to the *most recently requested*
                // entry with this call_id, matching `AgentFrame::
                // tool_call_request`'s convention (`frame.rs`). A provider
                // can legitimately reuse a call_id for a second, distinct
                // call after the first one's full request/approve/finish
                // cycle already closed (observed 2026-07-18: a rig/
                // Kimi-K2.7-Code turn re-requested `fs.edit` with the same
                // id an already-finished call had used). Forward `.find()`
                // would keep re-attributing every follow-up event to that
                // stale first entry, leaving the real, currently-pending
                // occurrence permanently unresolved (`ApprovalState::None`,
                // no Approve/Deny row ever rendered -- the "session wedged
                // on an empty edit call" report).
                if let Some(entry) = building
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.call_id == request.call_id)
                {
                    entry.had_approval_request = true;
                }
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                if let Some(entry) = building
                    .iter_mut()
                    .rev()
                    .find(|entry| &entry.call_id == call_id)
                {
                    entry.started = true;
                }
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(entry) = building
                    .iter_mut()
                    .rev()
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
/// per-tool body the completed-turn receipt's own expansion already shows.
/// A still-running call stays non-expandable: it has no result yet to show
/// a body for. A `Waiting` call (has an unresolved approval) is also
/// unfinished by this same rule, so it's covered without a separate
/// check -- it already auto-shows its proposal body unconditionally
/// (`AgentView::render_waiting_proposal`), untouched by this predicate.
pub fn running_row_expandable(call: &ToolCallView) -> bool {
    call.finished
}

/// Whether `call_id`'s approval request is still unresolved within
/// `turn_items` -- a single turn's own item slice is enough to answer
/// this without consulting the whole frame: every tool call this crate
/// emits, Horizon-executed or provider-forwarded, resolves via a
/// `ToolCallStarted` or `ToolCallFinished` with the same `call_id` (see
/// `crate::tools::approval::resolve_approval`, the one path every
/// approve/deny decision funnels through -- an approve folds
/// `ToolCallStarted` immediately, `ToolCallFinished` too if the tool runs
/// synchronously; a deny folds `ToolCallFinished` alone) before its turn
/// can end in the normal case, so the resolving item -- if any -- already
/// lives in the same span as the request. A turn that ends with a
/// still-pending approval (e.g. `Halted`) is the shouldn't-happen case
/// this stays `true` for, so a completed turn still renders it rather
/// than silently dropping it (`docs/agent-output-ui-amendment.md` stage
/// C's owner-reported fold bug: answered approvals must fold into the
/// receipt like any other tool activity, not linger as boxes forever).
pub fn is_approval_still_pending(turn_items: &[AgentFrameItem], call_id: &ToolCallId) -> bool {
    pending_approval_call_ids_in(turn_items).contains(call_id)
}

/// `(finished, total)` tool-call counts for a running card's `n / m`
/// progress header.
pub fn progress(tool_calls: &[ToolCallView]) -> (usize, usize) {
    let finished = tool_calls.iter().filter(|call| call.finished).count();
    (finished, tool_calls.len())
}

/// Maps a tool id to its display verb, target, (would-be) result
/// summary, and any tool-specific structured data -- the one place that
/// knows the exact input/output JSON shape each tool in
/// `crate::tools` uses (see that module's `tools/fs`, `tools/bash`
/// submodules). Unknown tool ids fall back to the raw id as the verb with
/// no target/summary, so a future tool renders *something* sane rather
/// than nothing.
///
/// Public (not just crate-internal) because `src/agent/turns`'s own
/// `terse_summary` -- a wording function that stayed behind in the
/// `horizon` binary crate -- reuses this classifier's verb/target/summary
/// for every tool id it doesn't special-case itself (see `transcript`'s
/// module doc for why this one didn't cleanly split).
pub fn classify(
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

/// Reads a string field out of a tool's input/output JSON. Public so
/// `src/agent/turns`'s `terse_summary` (which stayed behind, see
/// [`classify`]'s doc comment) can read the same fields [`classify`]
/// does without duplicating the extraction.
pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
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
/// call has today (see `crate::tools::fs::edit`). Derived from
/// [`super::reconstruct_line_diff`] rather than computed independently, so
/// the receipt chip's counts and the expanded body's diff can never drift
/// apart.
fn line_diffstat(old: &str, new: &str) -> (u32, u32) {
    let lines = super::reconstruct_line_diff(old, new);
    let added = lines
        .iter()
        .filter(|line| line.kind == super::DiffLineKind::Added)
        .count() as u32;
    let removed = lines
        .iter()
        .filter(|line| line.kind == super::DiffLineKind::Removed)
        .count() as u32;
    (added, removed)
}

/// Caps `lines` to its first `max_lines` entries, returning `(kept,
/// omitted)` -- used wherever the head of the content matters most (diff
/// bodies, content previews, the raw-JSON fallback -- all in
/// `src/agent/turns`'s `build_tool_call_body`). Public: wording-free line
/// capping, reused across the crate boundary by that function.
pub fn cap_lines_head<T>(mut lines: Vec<T>, max_lines: usize) -> (Vec<T>, usize) {
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
pub fn cap_lines_tail(mut lines: Vec<String>, max_lines: usize) -> (Vec<String>, usize) {
    if lines.len() <= max_lines {
        (lines, 0)
    } else {
        let omitted = lines.len() - max_lines;
        let kept = lines.split_off(lines.len() - max_lines);
        (kept, omitted)
    }
}

/// A streaming reasoning ("thinking") block's line cap -- kept small,
/// deliberately quieter and more compact than a tool-call body's own caps:
/// thinking is meant to read as a quiet side-channel while it streams, not
/// a large panel competing with assistant prose for the transcript's
/// vertical space.
pub const THINKING_TAIL_LINES: usize = 6;

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
pub fn cap_thinking_text(text: &str, max_lines: usize) -> (String, usize) {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (kept, omitted) = cap_lines_tail(lines, max_lines);
    (kept.join("\n"), omitted)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::*;

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
}
