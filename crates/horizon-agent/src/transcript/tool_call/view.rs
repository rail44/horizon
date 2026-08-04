use crate::contract::{OccurrenceId, ToolCallId, ToolCallResult};
use crate::frame::{pending_approval_call_ids_in, AgentFrameItem};
use serde_json::Value;

use super::approval::{derive_approval_state, is_superseded_output};
use super::classify::classify;
use super::files::affected_files;

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
