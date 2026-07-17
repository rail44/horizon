//! One tool call's view-model: classification into a display verb/target/
//! summary, approval-lifecycle derivation, and the expanded per-tool body
//! (diff/content-preview/command/summary/raw-JSON) shown when a row or
//! receipt chip expands.

use horizon_agent::contract::{ToolCallId, ToolCallResult};
use horizon_agent::frame::{pending_approval_call_ids_in, AgentFrameItem};
use serde_json::Value;

use super::diff::{reconstruct_line_diff, DiffLine, DiffLineKind};
use super::file_name;

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
}
