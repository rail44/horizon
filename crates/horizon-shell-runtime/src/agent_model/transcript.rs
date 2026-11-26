//! The structural reading of a folded frame: turn spans, tool-activity
//! bursts, per-call views, and the receipt aggregation, from what ACP's
//! tool call fields say: the title, the kind, the files.

use std::collections::HashSet;
use std::time::Duration;

use super::types::*;

/// One tool burst within a turn: a maximal run of tool activity, `[start,
/// end)` of the turn's items. A burst that has closed never reopens, so a
/// turn can fold into several receipts as it goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Burst {
    pub start: usize,
    pub end: usize,
}

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
    /// The same position once the call has finished.
    pub result_index: Option<usize>,
    pub title: String,
    pub kind: ToolKind,
    /// The files it works on: those its change names, else its locations.
    pub files: Vec<String>,
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

/// The files a call works on: those its change names, else its locations.
fn files_of(call: &ToolCall) -> Vec<String> {
    let changed: Vec<String> = (call.content.iter())
        .flat_map(|content| match content {
            ToolContent::Diff { changes, .. } => {
                changes.iter().map(|(path, _)| path.clone()).collect()
            }
            ToolContent::Text(_) => Vec::new(),
        })
        .collect();
    match changed.is_empty() {
        true => call.locations.clone(),
        false => changed,
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
            ToolCallView {
                occurrence_id: call.occurrence_id.clone(),
                request_index: index,
                result_index: outcome.is_some().then_some(index),
                title: call.title.clone(),
                kind: call.kind,
                files: files_of(call),
                outcome,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallClass {
    Read,
    Edit,
    Run,
    Query,
}

fn class_of(kind: ToolKind) -> CallClass {
    match kind {
        ToolKind::Read => CallClass::Read,
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => CallClass::Edit,
        ToolKind::Execute => CallClass::Run,
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
        match class_of(call.kind) {
            CallClass::Edit => edited_paths.extend(call.files.iter().cloned()),
            CallClass::Run => aggregate.bash_count += 1,
            // A read names its file; one that names none is counted by its
            // title.
            CallClass::Read => match call.files.is_empty() {
                true => _ = read_paths.insert(call.title.clone()),
                false => read_paths.extend(call.files.iter().cloned()),
            },
            CallClass::Query => aggregate.query_count += 1,
        }
    }

    aggregate.read_file_count = read_paths.len();
    aggregate.edited_file_count = edited_paths.len();
    aggregate
}

/// The call item a view's `request_index` points at.
pub fn tool_call_source<'a>(
    items: &'a [AgentFrameItem],
    call: &ToolCallView,
) -> Option<&'a ToolCall> {
    let AgentFrameItem::ToolCall(item) = items.get(call.request_index)? else {
        return None;
    };
    (item.occurrence_id == call.occurrence_id).then_some(item.as_ref())
}
