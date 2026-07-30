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

use crate::contract::{OccurrenceId, ToolCallId, ToolCallResult};
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
        /// `(added, removed)` line counts, derived from the `old_string`/
        /// `new_string` pairs of `fs.edit`'s `edits` list (summed when the
        /// call carries several edits for the one file). `None` when not
        /// derivable (e.g. `fs.write`, which replaces wholesale rather
        /// than diffing).
        diffstat: Option<(u32, u32)>,
    },
    Bash {
        command_head: String,
    },
}

/// One filesystem path affected by a successful mutation. A separate list
/// is necessary because one `fs.edit` call carries a whole batch of edits
/// behind a single receipt row and may change many files; its display
/// target alone cannot preserve that cardinality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffect {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub created: bool,
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
    pub affected_files: Vec<FileEffect>,
    pub finished: bool,
    pub is_error: bool,
    /// This row is a denial-retry attempt that was abandoned: it ran, was
    /// refused a domain or a path, and an approved retry took its place, so
    /// its terminal result is the [`SUPERSEDED_BY_RETRY`] marker rather
    /// than the outcome the model ever saw (backlog 55; the marker is
    /// written by `crate::tools::approval`'s denial-retry approve path).
    /// Neither success nor failure -- the renderers give it a muted
    /// "superseded" register instead of the success check or the error
    /// cross.
    pub superseded: bool,
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

/// Output-JSON marker key for an abandoned denial-retry attempt's terminal
/// result -- see [`ToolCallView::superseded`]. Defined here, next to
/// [`is_denied_output`]'s convention, and used by the one writer
/// (`crate::tools::approval::superseded_by_retry_result`) so the key exists
/// exactly once.
pub const SUPERSEDED_BY_RETRY: &str = "superseded_by_retry";

/// The display register an abandoned attempt's row reports instead of a
/// tool-specific summary.
pub const SUPERSEDED_SUMMARY: &str = "superseded by retry";

/// Whether `output` is an abandoned denial-retry attempt's terminal result.
pub fn is_superseded_output(output: &Value) -> bool {
    output.get(SUPERSEDED_BY_RETRY).and_then(Value::as_bool) == Some(true)
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
        /// Per-occurrence identity from the originating `ToolCallRequest`.
        /// `None` for legacy / replayed logs that pre-date the field; the
        /// matching logic below falls back to `.rev()`-by-call_id in that
        /// case (the prior behavior, retained for replay correctness).
        occurrence_id: Option<OccurrenceId>,
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
                    occurrence_id: request.occurrence_id.clone(),
                    tool_id: &request.tool_id,
                    input: &request.input,
                    result: None,
                    had_approval_request: false,
                    started: false,
                });
            }
            AgentFrameItem::ApprovalRequested(request) => {
                // Attribute to the entry that shares this approval's
                // `occurrence_id` first (the per-occurrence identity the
                // agentd stamps on every reissue, see
                // `session/approval.rs::begin_reissued_approval`). For
                // legacy `None` approvals (or replayed pre-feature logs)
                // fall back to the prior `.rev()`-by-call_id semantic --
                // attribute to the most recently requested entry with this
                // call_id, matching `AgentFrame::tool_call_request`'s
                // convention (`frame.rs`). A provider can legitimately
                // reuse a call_id for a second, distinct call after the
                // first one's full request/approve/finish cycle already
                // closed (observed 2026-07-18: a rig/Kimi-K2.7-Code turn
                // re-requested `fs.edit` with the same id an
                // already-finished call had used). Forward `.find()` would
                // keep re-attributing every follow-up event to that stale
                // first entry, leaving the real, currently-pending
                // occurrence permanently unresolved (`ApprovalState::
                // None`, no Approve/Deny row ever rendered -- the "session
                // wedged on an empty edit call" report). The
                // `occurrence_id` match wins on top of that for the
                // sandbox-denial-retry shape (same call_id, two distinct
                // `ApprovalRequested` events for the same conceptual
                // call's two attempts).
                let entry = match &request.occurrence_id {
                    Some(occ) => building
                        .iter_mut()
                        .rev()
                        .find(|entry| entry.occurrence_id.as_ref() == Some(occ)),
                    None => building
                        .iter_mut()
                        .rev()
                        .find(|entry| entry.call_id == request.call_id),
                };
                if let Some(entry) = entry {
                    entry.had_approval_request = true;
                }
            }
            AgentFrameItem::ToolCallStarted(call_id) => {
                // `ToolCallStarted(ToolCallId)` carries no `occurrence_id`
                // (it stays a unit-style variant to keep the wire change
                // additive -- see `contract.rs`'s doc comment on
                // `OccurrenceId`). It still uses the prior `.rev()`-
                // by-call_id semantic; this is correct because the
                // agentd never reissues the same call_id's started
                // signal without reissuing the request first, so the most
                // recently requested entry is always the right target.
                if let Some(entry) = building
                    .iter_mut()
                    .rev()
                    .find(|entry| &entry.call_id == call_id)
                {
                    entry.started = true;
                }
            }
            AgentFrameItem::ToolCallFinished(result) => {
                // Match by `occurrence_id` first -- this is the fix for
                // both shapes the user observed:
                //
                // * provider-reuse: provider emits a fresh
                //   `ToolCallRequested` with the same `call_id` as an
                //   already-finished call. Each request gets its own
                //   `Building` entry (with its own `occurrence_id`), and
                //   the result lands on the entry whose `occurrence_id`
                //   matches, not on the most recent request of that
                //   call_id (which on provider-reuse is the *new* request,
                //   the one the result does not answer to).
                // * sandbox-denial-retry: agentd's
                //   `begin_reissued_approval` reissues the request with
                //   a fresh `occurrence_id` (see
                //   `session/approval.rs`). The first attempt's result
                //   (denied or otherwise) attaches to the first entry, the
                //   second attempt's result to the second -- so the
                //   transcript shows both attempts visible as one
                //   conceptual call with two occurrences, as the user
                //   requested.
                //
                // The `.rev()` fallback for `None` preserves replay
                // correctness for events persisted before this field
                // existed.
                let entry = match &result.occurrence_id {
                    Some(occ) => building
                        .iter_mut()
                        .rev()
                        .find(|entry| entry.occurrence_id.as_ref() == Some(occ)),
                    None => building
                        .iter_mut()
                        .rev()
                        .find(|entry| entry.call_id == result.call_id),
                };
                if let Some(entry) = entry {
                    entry.result = Some(result);
                }
            }
            _ => {}
        }
    }

    building
        .into_iter()
        .map(|entry| {
            let output = entry.result.map(|result| &result.output.0);
            let (verb, target, result_summary, kind) = classify(entry.tool_id, entry.input, output);
            let affected_files = affected_files(entry.tool_id, entry.input, output);
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
                affected_files,
                finished: entry.result.is_some(),
                is_error: entry.result.map(|result| result.is_error).unwrap_or(false),
                superseded: output.is_some_and(is_superseded_output),
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
    let (verb, target, summary, kind) = classify_tool(tool_id, input, output);
    // An abandoned denial-retry attempt's result carries only the
    // superseded marker, so every tool-specific summary below reads
    // `None` off it (no `exit_code`, no counts). Say what happened
    // instead of leaving the row bare -- the row is closed, but neither
    // succeeded nor failed.
    let summary = if output.is_some_and(is_superseded_output) {
        Some(SUPERSEDED_SUMMARY.to_string())
    } else {
        summary
    };
    (verb, target, summary, kind)
}

fn classify_tool(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> (String, Option<String>, Option<String>, ToolCallKind) {
    match tool_id {
        "fs.edit" => {
            let edits = edit_entries(input);
            let paths = distinct_edit_paths(&edits);
            let diffstat = edits.iter().fold((0, 0), |(added, removed), edit| {
                let (edit_added, edit_removed) = line_diffstat(edit.old_string, edit.new_string);
                (added + edit_added, removed + edit_removed)
            });
            // One edit still reads as the file it touches; a batch reads as
            // its own cardinality, since no single path represents it.
            let target = match (edits.len(), paths.len()) {
                (0, _) => None,
                (1, _) => Some(edits[0].path.to_string()),
                (edit_count, file_count) => Some(format!(
                    "{edit_count} edits in {file_count} {}",
                    if file_count == 1 { "file" } else { "files" }
                )),
            };
            let kind = match paths.as_slice() {
                [path] => ToolCallKind::File {
                    file_name: file_name(path),
                    diffstat: Some(diffstat),
                },
                _ => ToolCallKind::Generic,
            };
            let (added, removed) = diffstat;
            (
                "Edit".to_string(),
                target,
                Some(format!("+{added} -{removed}")),
                kind,
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
        // `task`'s `description` input is a short label the model writes
        // for exactly this row (`tools::catalog`) -- it is the only place a
        // delegated task announces what it is doing while it runs, since
        // the session it spawns is deliberately kept out of the
        // client-visible session list (`roles::is_exploration`).
        //
        // Since the 2026-07-28 asynchronous cutover the call itself only
        // *launches* the task, so the honest summary is the launch
        // receipt's own `status` ("started"). The completed report is not
        // this call's result at all -- it arrives later as a
        // `MessageRole::TaskNotification` message in the transcript, and
        // `task_output`'s own row (below) reports "running" vs "finished"
        // for a task looked up afterwards.
        "task" => {
            let description = str_field(input, "description")
                .unwrap_or_default()
                .to_string();
            let summary = output.and_then(|output| str_field(output, "status").map(str::to_string));
            (
                "Task".to_string(),
                Some(description),
                summary,
                ToolCallKind::Generic,
            )
        }
        "task_output" => {
            // The label the launch recorded, echoed back by the fetch so
            // this row reads like the launch row rather than a bare uuid.
            let target = output
                .and_then(|output| str_field(output, "description"))
                .or_else(|| str_field(input, "session_id"))
                .unwrap_or_default()
                .to_string();
            let summary = output.and_then(|output| str_field(output, "status").map(str::to_string));
            (
                "Task Output".to_string(),
                Some(target),
                summary,
                ToolCallKind::Generic,
            )
        }
        other => (other.to_string(), None, None, ToolCallKind::Generic),
    }
}

fn affected_files(tool_id: &str, input: &Value, output: Option<&Value>) -> Vec<FileEffect> {
    match tool_id {
        "fs.edit" => edit_entries(input)
            .into_iter()
            .map(|edit| {
                let (added, removed) = line_diffstat(edit.old_string, edit.new_string);
                FileEffect {
                    path: edit.path.to_string(),
                    added,
                    removed,
                    created: false,
                }
            })
            .collect(),
        "fs.write" => {
            let Some(output) = output else {
                return Vec::new();
            };
            let Some(path) = str_field(input, "path") else {
                return Vec::new();
            };
            vec![FileEffect {
                path: path.to_string(),
                added: 0,
                removed: 0,
                created: output.get("created").and_then(Value::as_bool) == Some(true),
            }]
        }
        _ => Vec::new(),
    }
}

/// One entry of an `fs.edit` call's `edits` list (`crate::tools::fs::edit`).
/// Public alongside [`edit_entries`] so `src/agent/turns`'s own body
/// renderer reads the batch exactly the way this classifier does.
pub struct EditEntry<'a> {
    pub path: &'a str,
    pub old_string: &'a str,
    pub new_string: &'a str,
}

/// Reads an `fs.edit` call's `edits` list in input order. An entry with no
/// `path` is skipped (there is nothing to display it against); a missing or
/// non-array `edits` yields an empty list, so a malformed call renders as an
/// empty batch rather than panicking.
pub fn edit_entries(input: &Value) -> Vec<EditEntry<'_>> {
    input
        .get("edits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| {
                    Some(EditEntry {
                        path: str_field(edit, "path")?,
                        old_string: str_field(edit, "old_string").unwrap_or_default(),
                        new_string: str_field(edit, "new_string").unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The distinct paths an `fs.edit` batch touches, in first-touch order.
fn distinct_edit_paths<'a>(edits: &[EditEntry<'a>]) -> Vec<&'a str> {
    let mut paths: Vec<&str> = Vec::new();
    for edit in edits {
        if !paths.contains(&edit.path) {
            paths.push(edit.path);
        }
    }
    paths
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
/// enough to report `+added -removed` for one `old_string`/`new_string`
/// replacement, the shape of every entry in an `fs.edit` batch (see
/// `crate::tools::fs::edit`); a whole call's counts are these summed
/// across its `edits` list. Derived from
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

    /// `task`'s `description` input is what the requester's transcript
    /// shows while a delegated task runs -- the session it spawns is
    /// withheld from the client-visible session list, so this row is the
    /// only place it announces itself. Since the launch became
    /// asynchronous, the row's summary is the launch receipt's own status.
    #[test]
    fn a_task_call_is_labelled_with_its_description() {
        let items = vec![
            tool_requested(
                "t",
                "task",
                json!({"description": "map the emit sites", "prompt": "where are they?"}),
            ),
            tool_finished(
                "t",
                json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "started"}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Task");
        assert_eq!(views[0].target.as_deref(), Some("map the emit sites"));
        assert_eq!(views[0].result_summary.as_deref(), Some("started"));
        assert!(!views[0].is_error);
    }

    /// The pull half of the same pair: `task_output` echoes the launch's
    /// label back so its row reads like the launch row, and distinguishes a
    /// task still running from one whose report is ready.
    #[test]
    fn a_task_output_call_reports_running_and_finished_distinctly() {
        let running = build_tool_call_views(&[
            tool_requested("o", "task_output", json!({"session_id": "3f2b"})),
            tool_finished(
                "o",
                json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "running"}),
            ),
        ]);
        assert_eq!(running[0].verb, "Task Output");
        assert_eq!(running[0].target.as_deref(), Some("map the emit sites"));
        assert_eq!(running[0].result_summary.as_deref(), Some("running"));
        assert!(!running[0].is_error);

        let finished = build_tool_call_views(&[
            tool_requested("o", "task_output", json!({"session_id": "3f2b"})),
            tool_finished(
                "o",
                json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "finished", "report": "session.rs:1747"}),
            ),
        ]);
        assert_eq!(finished[0].result_summary.as_deref(), Some("finished"));
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
                None,
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
                json!({"edits": [{"path": "a.rs", "old_string": "x", "new_string": "y"}]}),
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
                    "edits": [{
                        "path": "src/agent/view.rs",
                        "old_string": "line1\nold\nline3",
                        "new_string": "line1\nnew a\nnew b\nline3",
                    }],
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
    fn an_fs_edit_batch_reads_as_its_own_cardinality_and_keeps_every_affected_file() {
        let items = vec![
            tool_requested(
                "a",
                "fs.edit",
                json!({
                    "edits": [
                        {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                        {"path": "/w/b.rs", "old_string": "x", "new_string": "y\nz"},
                        {"path": "/w/a.rs", "old_string": "p", "new_string": "q"},
                    ],
                }),
            ),
            tool_finished(
                "a",
                json!({
                    "applied_count": 3,
                    "file_count": 2,
                    "edits": [
                        {"index": 0, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
                        {"index": 1, "path": "/w/b.rs", "status": "applied", "occurrences": 1},
                        {"index": 2, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
                    ],
                }),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Edit");
        assert_eq!(views[0].target.as_deref(), Some("3 edits in 2 files"));
        // Summed across the batch: three replacements, one of which adds a
        // line.
        assert_eq!(views[0].result_summary.as_deref(), Some("+4 -3"));
        // No single file represents a multi-file batch, so it gets no file
        // chip.
        assert_eq!(views[0].kind, ToolCallKind::Generic);
        assert_eq!(
            views[0]
                .affected_files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/w/a.rs", "/w/b.rs", "/w/a.rs"],
        );
    }

    #[test]
    fn an_fs_edit_batch_within_one_file_keeps_its_file_chip() {
        let items = vec![tool_requested(
            "a",
            "fs.edit",
            json!({
                "edits": [
                    {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                    {"path": "/w/a.rs", "old_string": "p", "new_string": "q\nr"},
                ],
            }),
        )];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].target.as_deref(), Some("2 edits in 1 file"));
        match &views[0].kind {
            ToolCallKind::File {
                file_name,
                diffstat,
            } => {
                assert_eq!(file_name, "a.rs");
                assert_eq!(*diffstat, Some((3, 2)));
            }
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

    /// Provider-reuse shape -- the `functions.fs.edit:66` incident in
    /// session 05254b6a, generalized: a single provider `call_id` is
    /// legitimately used by two completely distinct tool calls. Without
    /// per-occurrence identity, both requests collapse onto the same
    /// `Building` slot and the first result attributes to the second
    /// request (or vice versa), leaving the genuine occurrence stuck
    /// "started-but-never-finished" in the transcript. See
    /// `backlog 42 / 55`.
    #[test]
    fn provider_reused_call_id_attributes_each_occurrence_to_its_own_result() {
        // Both requests land before either result -- the shape the
        // `.rev()` fallback actually gets wrong. A batched turn requests
        // two calls at once, and a provider that reuses ids hands both
        // the same `call_id`; with positional matching alone, `fin(occ_a)`
        // attributes to the *newest* entry with that call_id, which is
        // occurrence B. Each request carries its own fresh `OccurrenceId`,
        // exactly what `rig_tool_call_request` mints at the provider
        // boundary.
        let occ_a = OccurrenceId("occ-A".to_string());
        let occ_b = OccurrenceId("occ-B".to_string());
        let items = vec![
            tool_requested_with_occurrence(
                "fs.edit:1",
                "fs.edit",
                json!({"edits": [{"path": "a.txt", "old_string": "x", "new_string": "y"}]}),
                occ_a.clone(),
            ),
            tool_requested_with_occurrence(
                "fs.edit:1",
                "fs.edit",
                json!({"edits": [{"path": "b.txt", "old_string": "p", "new_string": "q"}]}),
                occ_b.clone(),
            ),
            // A's result arrives first and must land on A's row, not on
            // the more recent B.
            tool_finished_with_occurrence(
                "fs.edit:1",
                json!({ "is_error": false, "applied": true }),
                occ_a.clone(),
            ),
            tool_finished_with_occurrence(
                "fs.edit:1",
                json!({ "is_error": true, "message": "old_string not found" }),
                occ_b.clone(),
            ),
        ];
        let views = build_tool_call_views(&items);
        // Two rows, one per occurrence -- exactly what the user wants
        // for the provider-reuse shape.
        assert_eq!(views.len(), 2);
        // Each row's `call_id` is the same (the provider gave us the same
        // string twice); the second key decides which `Building` entry
        // each result attached to. Attribution is observable two ways:
        // `affected_files` carries the path of the *request* the row was
        // built from, and `is_error` carries the outcome of the *result*
        // that attached to it -- so a misattribution pairs a.txt with B's
        // failure (and vice versa) rather than going unnoticed.
        assert_eq!(views[0].call_id, ToolCallId("fs.edit:1".to_string()));
        assert!(views[0].finished);
        assert_eq!(
            views[0]
                .affected_files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt"],
        );
        assert!(
            !views[0].is_error,
            "a.txt's row must carry occurrence A's successful result, not B's failure"
        );
        assert_eq!(views[1].call_id, ToolCallId("fs.edit:1".to_string()));
        assert!(views[1].finished);
        assert_eq!(
            views[1]
                .affected_files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["b.txt"],
        );
        assert!(
            views[1].is_error,
            "b.txt's row must carry occurrence B's failing result"
        );
    }

    /// Domain-denial-retry shape, in the sequence the daemon actually
    /// emits (`crates/horizon-agentd/src/session/completion.rs`'s
    /// `fold_domain_denied` plus `tools::approval::
    /// resolve_domain_denial_retry`): a tier-1 auto-approved bash call
    /// starts, is refused a domain, and is *reissued* under the same
    /// `call_id` with a fresh occurrence and its own approval prompt.
    /// `fold_domain_denied` emits no result for the first attempt -- it
    /// parks the outcome on the approval's `prior_result` -- so the only
    /// `ToolCallFinished` here arrives after the reissue. Denying it
    /// forwards that parked result, stamped with the *first* attempt's
    /// occurrence, which is precisely where positional `.rev()` matching
    /// misfires: the newest request with this `call_id` is the reissue.
    /// See `backlog 42 / 55`.
    #[test]
    fn a_denial_retrys_parked_result_attaches_to_the_attempt_that_produced_it() {
        let occ_1 = OccurrenceId("occ-1".to_string());
        let occ_2 = OccurrenceId("occ-2".to_string());
        let items = vec![
            // Initial attempt: tier-1 auto-approved, so it starts with no
            // approval prompt of its own, and gets no result event.
            tool_requested_with_occurrence(
                "bash:1",
                "bash",
                json!({"command": "curl -sS http://evil.example.com/x"}),
                occ_1.clone(),
            ),
            tool_started("bash:1"),
            // `fold_domain_denied`'s reissue: same `call_id`, fresh
            // occurrence, and the retry prompt.
            tool_requested_with_occurrence(
                "bash:1",
                "bash",
                json!({"command": "curl -sS http://evil.example.com/x"}),
                occ_2.clone(),
            ),
            approval_requested_with_occurrence("bash:1", occ_2.clone()),
            // The user declined the domain grant, so the parked first
            // attempt's outcome is what reaches the provider -- carrying
            // `occ_1`, the occurrence that actually ran it.
            tool_finished_with_occurrence(
                "bash:1",
                json!({
                    "is_error": true,
                    "denied_domains": ["evil.example.com"],
                    "exit_code": 0,
                }),
                occ_1.clone(),
            ),
        ];
        let views = build_tool_call_views(&items);
        // Both attempts are visible -- two rows, same conceptual
        // `call_id` (the agentd reissues the id, only the occurrence is
        // fresh).
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].call_id, ToolCallId("bash:1".to_string()));
        assert_eq!(views[1].call_id, ToolCallId("bash:1".to_string()));
        // The result belongs to the attempt that ran, not to the reissue
        // that was declined.
        assert!(
            views[0].finished && views[0].is_error,
            "the parked outcome must land on the first attempt's row"
        );
        assert!(
            !views[1].finished,
            "the declined reissue produced no result of its own"
        );
        // The reissue is the row that carries the approval, and it
        // resolved as a denial (its prompt was answered, and no
        // `ToolCallStarted` followed).
        assert_eq!(views[0].approval, ApprovalState::None);
        assert_eq!(views[1].approval, ApprovalState::Waiting);
        // The deny path was already the one that closed the first row; the
        // approve path's counterpart is
        // `an_approved_denial_retry_closes_the_abandoned_attempt_as_superseded`
        // below (backlog 55).
    }

    /// The same denial-retry shape, taken down the *approve* branch --
    /// backlog 55's fix (owner decision 2026-07-28). The parked outcome is
    /// discarded there (the retry recomputes it), so
    /// `tools::approval::superseded_by_retry_result` closes the abandoned
    /// occurrence with a terminal marker instead, and the retry's own
    /// result closes the reissue. Both rows must read closed, and the
    /// abandoned one must read as *superseded* rather than as a success or
    /// a failure.
    #[test]
    fn an_approved_denial_retry_closes_the_abandoned_attempt_as_superseded() {
        let occ_1 = OccurrenceId("occ-1".to_string());
        let occ_2 = OccurrenceId("occ-2".to_string());
        let command = json!({"command": "cargo build --workspace"});
        let items = vec![
            tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
            tool_started("bash:1"),
            // `fold_filesystem_denied`'s reissue plus its retry prompt.
            tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
            approval_requested_with_occurrence("bash:1", occ_2.clone()),
            // Approve: the abandoned attempt is closed, the retry starts.
            tool_finished_with_occurrence(
                "bash:1",
                json!({
                    SUPERSEDED_BY_RETRY: true,
                    "retry_occurrence_id": occ_2.0,
                    "message": "this attempt was abandoned; an approved retry of the same call \
                                replaced it",
                }),
                occ_1.clone(),
            ),
            tool_started("bash:1"),
            // ... and finishes on its own.
            tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2.clone()),
        ];

        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);

        assert!(
            views[0].finished,
            "the abandoned attempt must no longer render started-but-never-finished"
        );
        assert!(views[0].superseded);
        assert!(
            !views[0].is_error,
            "an attempt a retry replaced did not fail on its own terms"
        );
        assert_eq!(views[0].result_summary.as_deref(), Some(SUPERSEDED_SUMMARY));
        assert_eq!(views[0].approval, ApprovalState::None);

        assert!(views[1].finished);
        assert!(!views[1].superseded);
        assert!(!views[1].is_error);
        assert_eq!(views[1].result_summary.as_deref(), Some("exit 0"));
        assert_eq!(views[1].approval, ApprovalState::Approved);
    }

    /// The retry's own result can land before the abandoned attempt's
    /// close in a replayed log; occurrence-first matching must keep each
    /// result on the row that produced it either way.
    #[test]
    fn a_superseded_close_arriving_after_the_retrys_result_still_lands_on_its_own_row() {
        let occ_1 = OccurrenceId("occ-1".to_string());
        let occ_2 = OccurrenceId("occ-2".to_string());
        let command = json!({"command": "cargo build --workspace"});
        let items = vec![
            tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
            tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
            tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2),
            tool_finished_with_occurrence("bash:1", json!({ SUPERSEDED_BY_RETRY: true }), occ_1),
        ];

        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);
        assert!(views[0].superseded && views[0].finished);
        assert!(!views[1].superseded && views[1].finished);
        assert_eq!(views[1].result_summary.as_deref(), Some("exit 0"));
    }

    /// A superseded attempt is a genuine attempt, not a failure and not an
    /// anomaly, so the collapsed receipt line must neither break it out as
    /// an individual chip nor count it alongside the retry that carries the
    /// real outcome.
    #[test]
    fn the_collapsed_receipt_counts_a_superseded_attempt_once_not_twice() {
        let occ_1 = OccurrenceId("occ-1".to_string());
        let occ_2 = OccurrenceId("occ-2".to_string());
        let command = json!({"command": "cargo build --workspace"});
        let views = build_tool_call_views(&[
            tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
            tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
            tool_finished_with_occurrence("bash:1", json!({ SUPERSEDED_BY_RETRY: true }), occ_1),
            tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2),
        ]);

        let aggregate = super::super::aggregate_receipt(&views);
        assert_eq!(aggregate.bash_count, 1);
        assert!(aggregate.individual_calls.is_empty());
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
