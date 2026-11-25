//! The structural reading of a folded frame: turn spans, tool-activity
//! bursts, per-call views, and the receipt
//! aggregation. The per-tool JSON readers
//! (`classify`, `affected_files`) are `horizon_agent::transcript`'s,
//! which take plain tool ids and JSON.

use std::collections::HashSet;
use std::time::Duration;

use horizon_agent::transcript::{FileEffect, ToolCallClassification, affected_files, classify};
use serde_json::Value;

use super::types::*;

pub use horizon_agent::transcript::Burst;

/// One turn's items, `[start, end)` into the frame's items. `ended` is
/// `None` for the turn still in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSpan {
    pub start: usize,
    pub end: usize,
    pub ended: Option<TurnEnd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    pub reason: TurnEndReason,
    pub elapsed: Duration,
}

/// Groups `items` into turn spans: a span opens at any item while none is
/// open and closes at the next `TurnEnded` (inclusive). Every item falls
/// in exactly one span; a trailing span without a `TurnEnded` is the turn
/// in progress.
pub fn group_into_turns(items: &[AgentFrameItem]) -> Vec<TurnSpan> {
    let mut spans = Vec::new();
    let mut current_start: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if current_start.is_none() {
            current_start = Some(index);
        }
        if let AgentFrameItem::TurnEnded { reason, elapsed } = item {
            let start = current_start.take().unwrap_or(index);
            spans.push(TurnSpan {
                start,
                end: index + 1,
                ended: Some(TurnEnd {
                    reason: *reason,
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

fn is_tool_related(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::ToolCall(_) | AgentFrameItem::Permission(_)
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
pub fn segment_bursts(items: &[AgentFrameItem]) -> Vec<Burst> {
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
            AgentFrameItem::TurnEnded { .. } | AgentFrameItem::Compaction(_) => true,
            _ => false,
        };
        if closes_burst {
            bursts.extend(open.take());
        }
    }

    bursts.extend(open);
    bursts
}

/// One tool call's view-model, shared by the running card's rows and the
/// receipt's chips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallView {
    pub occurrence_id: String,
    /// Position of the call's item in the slice passed to
    /// [`build_tool_call_views`].
    pub request_index: usize,
    /// The same position once the call has an output.
    pub result_index: Option<usize>,
    pub tool_id: String,
    pub verb: String,
    pub target: Option<String>,
    pub affected_files: Vec<FileEffect>,
    pub outcome: Option<ToolOutcome>,
}

impl ToolCallView {
    pub fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            occurrence_id: self.occurrence_id.clone(),
        }
    }

    pub fn finished(&self) -> bool {
        self.outcome.is_some()
    }

    pub fn is_success(&self) -> bool {
        self.outcome == Some(ToolOutcome::Succeeded)
    }
}

/// Builds one view per tool call in `items`, in first-seen order.
pub fn build_tool_call_views(items: &[AgentFrameItem]) -> Vec<ToolCallView> {
    // The calls whose approval the human denied.
    let denied: HashSet<&str> = (items.iter())
        .filter_map(|item| match item {
            AgentFrameItem::Permission(permission)
                if permission.decision == Some(PermissionDecision::Denied) =>
            {
                Some(permission.identity.occurrence_id.as_str())
            }
            _ => None,
        })
        .collect();
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            AgentFrameItem::ToolCall(call) => Some((index, call)),
            _ => None,
        })
        .map(|(index, call)| {
            let outcome = match call.outcome() {
                Some(ToolOutcome::Failed) if denied.contains(call.occurrence_id.as_str()) => {
                    Some(ToolOutcome::Denied)
                }
                outcome => outcome,
            };
            let output = call.output.as_ref().filter(|_| outcome.is_some());
            let ToolCallClassification { verb, target, .. } =
                classify(&call.tool_id, &call.input, output);
            let affected_files = match &outcome {
                Some(ToolOutcome::Succeeded | ToolOutcome::Failed) => {
                    affected_files(&call.tool_id, &call.input, output)
                }
                _ => Vec::new(),
            };
            ToolCallView {
                occurrence_id: call.occurrence_id.clone(),
                request_index: index,
                result_index: outcome.is_some().then_some(index),
                tool_id: call.tool_id.clone(),
                verb,
                target,
                affected_files,
                outcome,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallClass {
    Edit,
    Bash,
    Query,
}

fn classify_call(tool_id: &str) -> CallClass {
    match tool_id {
        "fs_edit" | "fs_write" => CallClass::Edit,
        "bash" => CallClass::Bash,
        _ => CallClass::Query,
    }
}

/// The collapsed receipt line's counts. Failed or unfinished calls stay
/// individual.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiptAggregate {
    pub query_count: usize,
    pub read_file_count: usize,
    pub edited_file_count: usize,
    pub bash_count: usize,
    pub individual_calls: Vec<ToolCallView>,
}

pub fn aggregate_receipt(tool_calls: &[ToolCallView]) -> ReceiptAggregate {
    let mut aggregate = ReceiptAggregate::default();
    let mut read_paths: HashSet<String> = HashSet::new();
    let mut edited_paths: HashSet<String> = HashSet::new();

    for call in tool_calls {
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
            CallClass::Query if call.tool_id == "fs_read" => {
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

/// The item a view's `request_index` points at, with its output when the
/// call has finished.
pub fn tool_call_source<'a>(
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
    Some((&item.tool_id, &item.input, output))
}
