//! The structural reading of a folded frame: turn spans, tool-activity
//! bursts, per-call views with their approval lifecycle, and the receipt
//! and file-change aggregations. The per-tool JSON readers
//! (`classify`, `edit_entries`, `reconstruct_line_diff`) are
//! `horizon_agent::transcript`'s, which take plain tool ids and JSON.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use horizon_acp::ToolOutcome;
use horizon_agent::transcript::{
    classify, edit_entries, reconstruct_line_diff, ApprovalState, DiffLineKind, FileChange,
    FileEffect, ToolCallClassification, ToolCallKind,
};
use serde_json::Value;

use super::types::*;

pub(crate) use horizon_agent::transcript::Burst;

/// The summary an abandoned retry attempt's row reports.
pub(crate) const SUPERSEDED_SUMMARY: &str = "superseded by retry";

/// One turn's items, `[start, end)` into the frame's items. `ended` is
/// `None` for the turn still in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) ended: Option<TurnEnd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnEnd {
    pub(crate) reason: TurnEndReason,
    pub(crate) model: Option<String>,
    pub(crate) elapsed: Duration,
}

/// Groups `items` into turn spans: a span opens at any item while none is
/// open and closes at the next `TurnEnded` (inclusive). Every item falls
/// in exactly one span; a trailing span without a `TurnEnded` is the turn
/// in progress.
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

/// Whether `items` holds a user-authored message.
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

/// The model of the latest ended turn that recorded one.
pub(crate) fn latest_turn_model(items: &[AgentFrameItem]) -> Option<&str> {
    items.iter().rev().find_map(|item| match item {
        AgentFrameItem::TurnEnded {
            model: Some(model), ..
        } => Some(model.as_str()),
        _ => None,
    })
}

fn is_tool_related(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::ToolCall(_)
            | AgentFrameItem::Permission(_)
            | AgentFrameItem::ToolCallPreparing(_)
    )
}

fn is_assistant_text(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(Message {
            role: MessageRole::Assistant,
            ..
        })
    )
}

/// Segments a turn's items into tool bursts. A burst opens at the first
/// tool-related item while none is open and absorbs further tool-related
/// items until assistant text arrives with every call in it finished, or
/// the turn end or a compaction divider is reached. A closed burst never
/// reopens; later tool activity opens a new one.
pub(crate) fn segment_bursts(items: &[AgentFrameItem]) -> Vec<Burst> {
    let mut bursts = Vec::new();
    let mut open: Option<Burst> = None;

    for (index, item) in items.iter().enumerate() {
        if is_tool_related(item) {
            match &mut open {
                Some(burst) => burst.end = index + 1,
                None => {
                    open = Some(Burst {
                        start: index,
                        end: index + 1,
                        closed: false,
                    });
                }
            }
            continue;
        }
        let closes_burst = match item {
            item if is_assistant_text(item) => open.as_ref().is_some_and(|burst| {
                build_tool_call_views(&items[burst.start..burst.end])
                    .iter()
                    .all(ToolCallView::finished)
            }),
            AgentFrameItem::TurnEnded { .. } | AgentFrameItem::HistoryCleared(_) => true,
            _ => false,
        };
        if closes_burst {
            if let Some(mut burst) = open.take() {
                burst.closed = true;
                bursts.push(burst);
            }
        }
    }

    bursts.extend(open);
    bursts
}

/// One tool call's view-model, shared by the running card's rows and the
/// receipt's chips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCallView {
    pub(crate) call_id: String,
    pub(crate) occurrence_id: String,
    /// Position of the call's item in the slice passed to
    /// [`build_tool_call_views`].
    pub(crate) request_index: usize,
    /// The same position once the call has an output.
    pub(crate) result_index: Option<usize>,
    pub(crate) tool_id: String,
    pub(crate) verb: String,
    pub(crate) target: Option<String>,
    pub(crate) result_summary: Option<String>,
    pub(crate) kind: ToolCallKind,
    pub(crate) affected_files: Vec<FileEffect>,
    pub(crate) outcome: Option<ToolOutcome>,
    pub(crate) approval: ApprovalState,
}

impl ToolCallView {
    pub(crate) fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: self.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.outcome.is_some()
    }

    pub(crate) fn is_error(&self) -> bool {
        matches!(
            self.outcome,
            Some(ToolOutcome::Failed | ToolOutcome::Denied)
        )
    }

    pub(crate) fn is_success(&self) -> bool {
        self.outcome == Some(ToolOutcome::Succeeded)
    }

    pub(crate) fn superseded(&self) -> bool {
        matches!(self.outcome, Some(ToolOutcome::Superseded { .. }))
    }
}

/// Builds one view per tool call in `items`, in first-seen order.
pub(crate) fn build_tool_call_views(items: &[AgentFrameItem]) -> Vec<ToolCallView> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            AgentFrameItem::ToolCall(call) => Some((index, call)),
            _ => None,
        })
        .map(|(index, call)| {
            let identity = call.identity();
            let permission = items.iter().rev().find_map(|item| match item {
                AgentFrameItem::Permission(permission) if permission.identity == identity => {
                    Some(permission)
                }
                _ => None,
            });
            let outcome = call.outcome();
            let output = call.output.as_ref().filter(|_| outcome.is_some());
            let ToolCallClassification {
                verb,
                target,
                summary,
                kind,
                ..
            } = classify(&call.meta.tool_id, &call.input, output);
            let affected_files = match &outcome {
                Some(ToolOutcome::Succeeded | ToolOutcome::Failed) => {
                    affected_files(&call.meta.tool_id, &call.input, output)
                }
                _ => Vec::new(),
            };
            let result_summary = match &outcome {
                Some(ToolOutcome::Superseded { .. }) => Some(SUPERSEDED_SUMMARY.into()),
                Some(ToolOutcome::Cancelled) => Some("cancelled".into()),
                Some(ToolOutcome::Denied) => Some("denied".into()),
                Some(ToolOutcome::Succeeded | ToolOutcome::Failed) => summary,
                None => None,
            };
            ToolCallView {
                call_id: identity.call_id,
                occurrence_id: identity.occurrence_id,
                request_index: index,
                result_index: outcome.is_some().then_some(index),
                tool_id: call.meta.tool_id.clone(),
                verb,
                target,
                result_summary,
                kind,
                affected_files,
                approval: derive_approval_state(
                    permission.map(|permission| permission.decision),
                    call.started(),
                    outcome.as_ref(),
                ),
                outcome,
            }
        })
        .collect()
}

/// `permission` is `None` for a call that never had a permission request on
/// this attachment, `Some(decision)` otherwise. A started call reads as
/// approved before its result arrives.
fn derive_approval_state(
    permission: Option<Option<PermissionDecision>>,
    started: bool,
    outcome: Option<&ToolOutcome>,
) -> ApprovalState {
    let Some(decision) = permission else {
        return ApprovalState::None;
    };
    if started {
        return ApprovalState::Approved;
    }
    match outcome {
        Some(ToolOutcome::Denied) => ApprovalState::Denied,
        Some(ToolOutcome::Cancelled) => ApprovalState::Cancelled,
        Some(ToolOutcome::Superseded { .. }) => ApprovalState::Superseded,
        Some(_) => ApprovalState::Approved,
        None => match decision {
            Some(PermissionDecision::Approved) => ApprovalState::Approved,
            Some(PermissionDecision::Denied) => ApprovalState::Denied,
            Some(PermissionDecision::Cancelled) => ApprovalState::Cancelled,
            None => ApprovalState::Waiting,
        },
    }
}

/// Whether `identity`'s permission request is still unanswered within
/// `turn_items`.
pub(crate) fn is_approval_still_pending(
    turn_items: &[AgentFrameItem],
    identity: &ToolCallIdentity,
) -> bool {
    super::queries::pending_approval_identities_in(turn_items).contains(identity)
}

/// `(finished, total)` tool-call counts for the running card's header.
pub(crate) fn progress(tool_calls: &[ToolCallView]) -> (usize, usize) {
    let finished = tool_calls.iter().filter(|call| call.finished()).count();
    (finished, tool_calls.len())
}

/// Any finished call expands to its body; an unfinished one has none yet.
pub(crate) fn running_row_expandable(call: &ToolCallView) -> bool {
    call.finished()
}

fn affected_files(tool_id: &str, input: &Value, output: Option<&Value>) -> Vec<FileEffect> {
    use horizon_agent::contract::tool_output::{decode, EditOutcome, FileEdits, FileWritten};
    let Some(output) = output else {
        return Vec::new();
    };
    match tool_id {
        "fs.edit" => decode::<FileEdits>(output)
            .map(|result| {
                let inputs = edit_entries(input);
                result
                    .edits
                    .into_iter()
                    .filter_map(|receipt| {
                        let EditOutcome::Applied { occurrences } = receipt.outcome else {
                            return None;
                        };
                        let edit = inputs
                            .get(receipt.index)
                            .filter(|edit| edit.path == receipt.path)?;
                        let (added, removed) = line_diffstat(edit.old_string, edit.new_string);
                        let occurrences = u32::try_from(occurrences).unwrap_or(u32::MAX);
                        Some(FileEffect {
                            path: receipt.path,
                            added: added.saturating_mul(occurrences),
                            removed: removed.saturating_mul(occurrences),
                            created: false,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "fs.write" => decode::<FileWritten>(output)
            .map(|result| {
                vec![FileEffect {
                    path: result.path,
                    added: 0,
                    removed: 0,
                    created: result.created,
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn line_diffstat(old: &str, new: &str) -> (u32, u32) {
    let lines = reconstruct_line_diff(old, new);
    let count = |kind| lines.iter().filter(|line| line.kind == kind).count() as u32;
    (count(DiffLineKind::Added), count(DiffLineKind::Removed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallClass {
    Edit,
    Bash,
    Query,
}

fn classify_call(tool_id: &str) -> CallClass {
    match tool_id {
        "fs.edit" | "fs.write" => CallClass::Edit,
        "bash" => CallClass::Bash,
        _ => CallClass::Query,
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

/// The collapsed receipt line's counts. Failed or unfinished calls stay
/// individual; superseded attempts are left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReceiptAggregate {
    pub(crate) query_count: usize,
    pub(crate) read_file_count: usize,
    pub(crate) edited_file_count: usize,
    pub(crate) bash_count: usize,
    pub(crate) individual_calls: Vec<ToolCallView>,
}

pub(crate) fn aggregate_receipt(tool_calls: &[ToolCallView]) -> ReceiptAggregate {
    let mut aggregate = ReceiptAggregate::default();
    let mut read_paths: HashSet<String> = HashSet::new();
    let mut edited_paths: HashSet<String> = HashSet::new();

    for call in tool_calls {
        if call.superseded() {
            continue;
        }
        if !call.is_success() {
            aggregate.individual_calls.push(call.clone());
            continue;
        }
        match classify_call(&call.tool_id) {
            CallClass::Edit => {
                for file in &call.affected_files {
                    edited_paths.insert(file.path.clone());
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

/// Sums recorded file effects of finished edit-class calls per path, in
/// first-touch order.
pub(crate) fn aggregate_changes(tool_calls: &[ToolCallView]) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    for call in tool_calls {
        if classify_call(&call.tool_id) != CallClass::Edit {
            continue;
        }
        for file in &call.affected_files {
            let entry = match changes
                .iter_mut()
                .position(|change| change.path == file.path)
            {
                Some(index) => &mut changes[index],
                None => {
                    changes.push(FileChange {
                        path: file.path.clone(),
                        file_name: file_name(&file.path),
                        added: 0,
                        removed: 0,
                        created: false,
                    });
                    changes.last_mut().expect("just pushed")
                }
            };
            entry.added += file.added;
            entry.removed += file.removed;
            entry.created |= file.created;
        }
    }
    changes
}

/// The item a view's `request_index` points at, with its output when the
/// call has finished.
pub(crate) fn tool_call_source<'a>(
    items: &'a [AgentFrameItem],
    call: &ToolCallView,
) -> Option<(&'a str, &'a Value, Option<&'a Value>)> {
    let AgentFrameItem::ToolCall(item) = items.get(call.request_index)? else {
        return None;
    };
    if item.occurrence_id != call.occurrence_id {
        return None;
    }
    let output = call.result_index.and(item.output.as_ref());
    Some((&item.meta.tool_id, &item.input, output))
}
