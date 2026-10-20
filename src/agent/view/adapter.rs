//! Builds the pane's display model from a live session's folded frame.
//!
//! The structure comes from the model's own readings of the frame (turn
//! spans, tool bursts, per-call views, the receipt aggregate) and the
//! per-tool bodies from `turns`; this module only words them for
//! [`display`](super::display).
//!
//! A turn that has ended is worded once: its units are kept in
//! [`SettledUnits`], which the pane holds across re-adaptations, so a
//! session notify re-words only the turn in progress. The frame's settled
//! revision says when the kept units are stale.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horizon_acp::{MemoryDigest, MemoryField, TaskProgressNotification, ToolOutcome};

use super::super::model::{
    actionable_pending_approval_identities_in, state_indicates_turn_in_flight, AgentFrameItem,
    AgentModel, MessageRole, SessionState, ToolCallIdentity, TurnEndReason, TurnSpan,
};
use super::super::session::AgentSession;
use super::super::turns::{
    build_tool_call_views, group_into_turns, humanize_duration, pluralize, receipt_summary,
    segment_bursts, str_field, tool_call_body, DiffLineKind, ToolCallBody, ToolCallView,
};
use super::chrome;
use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message, Notice,
    Receipt, Status, Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};

/// The display model, and the identity behind each approval id it carries.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Adapted {
    pub(crate) transcript: Transcript,
    pub(crate) approvals: HashMap<String, ToolCallIdentity>,
}

/// The units of the turns that have ended, as last worded. Held by the
/// pane and handed to every adaptation of the same session: an ended
/// turn's wording depends on its own items alone, and those are settled
/// until the frame's settled revision moves.
#[derive(Debug, Default)]
pub(crate) struct SettledUnits {
    revision: u64,
    /// One entry per ended turn span, in frame order: the span's item
    /// range and the units it reads as.
    spans: Vec<(Range<usize>, Vec<Rc<Unit>>)>,
}

impl SettledUnits {
    /// How many ended turns are held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }
}

pub(crate) fn transcript_from(session: &AgentSession, settled: &mut SettledUnits) -> Adapted {
    adapt(
        &session.model,
        session.runtime_unreachable(),
        unix_epoch_ms(),
        settled,
    )
}

/// [`transcript_from`] over the session's parts, with `now_ms` the wall
/// clock background tasks are aged against and `settled` the ended turns'
/// units from the previous adaptation of the same model.
pub(crate) fn adapt(
    model: &AgentModel,
    unreachable: bool,
    now_ms: u64,
    settled: &mut SettledUnits,
) -> Adapted {
    let frame = &model.frame;
    let state = frame.state();
    let pending = actionable_pending_approval_identities_in(&frame.items);
    let mut approvals = HashMap::new();
    for identity in &pending {
        approvals.insert(approval_id(identity), identity.clone());
    }
    let in_flight = state_indicates_turn_in_flight(state);
    let units = units(
        &frame.items,
        in_flight,
        &pending,
        settled,
        frame.settled_revision,
    );
    let composer = match pending.first() {
        Some(identity) => ComposerMode::Approval {
            id: approval_id(identity),
        },
        None if matches!(
            state,
            Some(SessionState::Running | SessionState::ToolRunning)
        ) =>
        {
            ComposerMode::Running
        }
        None => ComposerMode::Idle,
    };
    Adapted {
        transcript: Transcript {
            model: model
                .selection
                .as_ref()
                .map(|selection| selection.model.clone())
                .unwrap_or_default(),
            status: status(state, unreachable),
            units,
            tasks: model
                .tasks
                .iter()
                .map(|task| task_line(task, now_ms))
                .collect(),
            composer,
        },
        approvals,
    }
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// The opaque id an approval carries through the view.
fn approval_id(identity: &ToolCallIdentity) -> String {
    identity.occurrence_id.clone()
}

fn status(state: Option<SessionState>, unreachable: bool) -> Status {
    if unreachable {
        return Status::Unreachable;
    }
    match state {
        Some(SessionState::Running) => Status::Running,
        Some(SessionState::ToolRunning) => Status::ToolRunning,
        Some(SessionState::WaitingForApproval) => Status::WaitingForApproval,
        Some(SessionState::Terminated) => Status::Terminated,
        Some(SessionState::Idle) | None => Status::Idle,
    }
}

fn task_line(task: &TaskProgressNotification, now_ms: u64) -> Task {
    Task {
        description: task.description.clone(),
        elapsed: humanize_duration(Duration::from_millis(
            now_ms.saturating_sub(task.started_at_epoch_ms),
        )),
        activity: task
            .activity
            .clone()
            .unwrap_or_else(|| "running".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// Every unit of the frame, in order: each turn span's units
/// ([`span_units`]), with an ended span's taken from `settled` when it was
/// worded before under the same `revision`, and kept there once worded.
fn units(
    items: &[AgentFrameItem],
    in_flight: bool,
    pending: &[ToolCallIdentity],
    settled: &mut SettledUnits,
    revision: u64,
) -> Vec<Rc<Unit>> {
    if settled.revision != revision {
        settled.spans.clear();
        settled.revision = revision;
    }
    let mut units = Vec::new();
    for (position, span) in group_into_turns(items).into_iter().enumerate() {
        if span.ended.is_none() {
            units.extend(
                span_units(items, &span, in_flight, pending)
                    .into_iter()
                    .map(Rc::new),
            );
            continue;
        }
        let range = span.start..span.end;
        if let Some((held, kept)) = settled.spans.get(position) {
            if *held == range {
                units.extend(kept.iter().cloned());
                continue;
            }
        }
        settled.spans.truncate(position);
        let worded: Vec<Rc<Unit>> = span_units(items, &span, false, pending)
            .into_iter()
            .map(Rc::new)
            .collect();
        units.extend(worded.iter().cloned());
        settled.spans.push((range, worded));
    }
    units
}

/// One turn span's units. A notice that arrives before the turn's first
/// output precedes its card and one that arrives after follows it. A
/// message sent into the session after the first output closes the card
/// so far (done, without elapsed time) and starts the next one; the span's
/// end state and elapsed time go to the last card. `in_flight` is the
/// session's state, which only the open span reads.
fn span_units(
    items: &[AgentFrameItem],
    span: &TurnSpan,
    in_flight: bool,
    pending: &[ToolCallIdentity],
) -> Vec<Unit> {
    let mut units = Vec::new();
    let turn_items = &items[span.start..span.end];
    let bursts = segment_bursts(turn_items);
    let running = span.ended.is_none() && in_flight;
    let ended = span.ended.is_some();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut parts = Vec::new();
    // The call views behind the last receipt in `parts`.
    let mut receipt_views = Vec::new();
    let mut started = false;
    for (index, item) in turn_items.iter().enumerate() {
        if let Some(burst) = bursts.iter().find(|burst| burst.start == index) {
            let key = span.start + burst.start;
            if let Some((receipt, views)) =
                receipt(key, &turn_items[burst.start..burst.end], ended, pending)
            {
                match parts.last_mut() {
                    Some(TurnPart::Receipt(previous)) => {
                        receipt_views.extend(views);
                        merge_receipt(previous, receipt, &receipt_views);
                    }
                    _ => {
                        receipt_views = views;
                        parts.push(TurnPart::Receipt(receipt));
                    }
                }
                started = true;
            }
        }
        match item {
            AgentFrameItem::Message(message) => match message.role {
                MessageRole::Assistant => {
                    if !message.text.is_empty() {
                        parts.push(TurnPart::Text(message.text.clone()));
                        started = true;
                    }
                }
                role => {
                    if started {
                        units.append(&mut before);
                        units.push(Unit::Turn(Turn {
                            state: TurnState::Done,
                            parts: std::mem::take(&mut parts),
                            elapsed: None,
                        }));
                        units.append(&mut after);
                        started = false;
                    }
                    before.push(Unit::User(Message {
                        label: label(role),
                        text: message.text.clone(),
                    }));
                }
            },
            AgentFrameItem::Thought(_) => started = true,
            item => {
                let side = if started { &mut after } else { &mut before };
                side.extend(notice(item).map(Unit::Notice));
            }
        }
    }
    let has_text = parts.iter().any(|part| matches!(part, TurnPart::Text(_)));
    let thinking_tail = matches!(turn_items.last(), Some(AgentFrameItem::Thought(_)));
    if running && (thinking_tail || !has_text) {
        parts.push(TurnPart::Thinking);
    }
    let state = match &span.ended {
        Some(end) => turn_state(end.reason),
        None if running => TurnState::Running,
        None => TurnState::Done,
    };
    let failed_end = !matches!(state, TurnState::Done | TurnState::Running);
    units.extend(before);
    if started || running || failed_end {
        units.push(Unit::Turn(Turn {
            state,
            parts,
            elapsed: span
                .ended
                .as_ref()
                .map(|end| humanize_duration(end.elapsed)),
        }));
    }
    units.extend(after);
    units
}

fn label(role: MessageRole) -> Label {
    match role {
        MessageRole::TaskNotification => Label::Task,
        MessageRole::AutoContinue => Label::Continue,
        MessageRole::User | MessageRole::Assistant => Label::You,
    }
}

fn turn_state(reason: TurnEndReason) -> TurnState {
    match reason {
        TurnEndReason::Completed => TurnState::Done,
        TurnEndReason::Cancelled => TurnState::Stopped,
        TurnEndReason::Failed
        | TurnEndReason::HaltedByIterationCap
        | TurnEndReason::HaltedByDoomLoop => TurnState::Failed,
    }
}

/// The line a system item reads as; `None` for an item that is not one.
fn notice(item: &AgentFrameItem) -> Option<Notice> {
    let (tone, text) = match item {
        AgentFrameItem::Error(text) | AgentFrameItem::Exited(text) => (Tone::Danger, text.clone()),
        AgentFrameItem::ProviderRateLimited(limited) => (
            Tone::Warning,
            format!(
                "Provider rate limited (attempt {}, retrying in {}s)",
                limited.attempt,
                limited.backoff_ms.div_ceil(1000)
            ),
        ),
        AgentFrameItem::HistoryCleared(cleared) => (
            Tone::Muted,
            format!(
                "History compacted: {} ({} chars) reclaimed",
                pluralize(
                    cleared.cleared_occurrence_ids.len(),
                    "tool output",
                    "tool outputs"
                ),
                grouped(cleared.recovered_chars)
            ),
        ),
        AgentFrameItem::MemoryDigest(digest) => (Tone::Muted, memory_digest(digest)),
        AgentFrameItem::MemoryCheckpointMissed => {
            (Tone::Warning, "Memory checkpoint missed".to_string())
        }
        _ => return None,
    };
    Some(Notice { tone, text })
}

fn memory_digest(digest: &MemoryDigest) -> String {
    if let Some(reason) = &digest.no_update_reason {
        return format!("Memory not updated: {reason}");
    }
    let mut fields: Vec<&str> = Vec::new();
    for update in &digest.updates {
        let field = memory_field(update.field);
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    if fields.is_empty() {
        "Memory digested".to_string()
    } else {
        format!("Memory updated: {}", fields.join(", "))
    }
}

fn memory_field(field: MemoryField) -> &'static str {
    match field {
        MemoryField::Goal => "goal",
        MemoryField::Decisions => "decisions",
        MemoryField::Completed => "completed",
        MemoryField::InProgress => "in progress",
        MemoryField::Stuck => "stuck",
        MemoryField::NextStep => "next step",
        MemoryField::Related => "related",
    }
}

/// `31204` as `31,204`.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

// ---------------------------------------------------------------------------
// Receipts and calls
// ---------------------------------------------------------------------------

/// One burst as a receipt, with the call views it was built from; `None`
/// when it holds no call to show.
fn receipt(
    key: usize,
    items: &[AgentFrameItem],
    turn_ended: bool,
    pending: &[ToolCallIdentity],
) -> Option<(Receipt, Vec<ToolCallView>)> {
    let views: Vec<ToolCallView> = build_tool_call_views(items)
        .into_iter()
        .filter(|view| !view.superseded())
        .collect();
    if views.is_empty() {
        return None;
    }
    let calls: Vec<Call> = views
        .iter()
        .map(|view| call(items, view, turn_ended, pending))
        .collect();
    let receipt = Receipt {
        key,
        summary: receipt_summary(&views),
        running: calls.iter().any(|call| call.status == CallStatus::Running),
        failed: calls.iter().any(|call| call.status == CallStatus::Failed),
        calls,
    };
    Some((receipt, views))
}

/// Appends `next`'s calls to `receipt`, which keeps its key, and
/// summarizes `views`, every call view behind both.
fn merge_receipt(receipt: &mut Receipt, next: Receipt, views: &[ToolCallView]) {
    receipt.calls.extend(next.calls);
    receipt.summary = receipt_summary(views);
    receipt.running |= next.running;
    receipt.failed |= next.failed;
}

fn call(
    items: &[AgentFrameItem],
    view: &ToolCallView,
    turn_ended: bool,
    pending: &[ToolCallIdentity],
) -> Call {
    let (verb, target) = verb_and_target(items, view);
    let status = match &view.outcome {
        None if turn_ended => CallStatus::Cancelled,
        None => CallStatus::Running,
        Some(ToolOutcome::Succeeded) => CallStatus::Done,
        Some(ToolOutcome::Failed) => CallStatus::Failed,
        Some(ToolOutcome::Denied) => CallStatus::Denied,
        Some(ToolOutcome::Cancelled | ToolOutcome::Superseded { .. }) => CallStatus::Cancelled,
    };
    let identity = view.identity();
    let approval = pending.contains(&identity).then(|| Approval {
        id: approval_id(&identity),
        reason: permission_reason(items, &identity).unwrap_or_default(),
    });
    Call {
        verb: verb.to_string(),
        target,
        status,
        body: body(items, view),
        approval,
    }
}

fn verb_and_target(items: &[AgentFrameItem], view: &ToolCallView) -> (&'static str, String) {
    let target = || view.target.clone().unwrap_or_default();
    match view.tool_id.as_str() {
        "fs.read" => ("Read", target()),
        "fs.edit" => ("Edit", target()),
        "fs.write" => ("Write", target()),
        "bash" => {
            let command = tool_input(items, view)
                .and_then(|input| str_field(input, "command"))
                .and_then(|command| command.lines().next())
                .map(str::to_string);
            ("Run", command.unwrap_or_else(target))
        }
        "fs.grep" | "fs.glob" => ("Search", target()),
        _ => (
            "Call",
            match &view.target {
                Some(target) => format!("{} {target}", view.verb),
                None => view.verb.clone(),
            },
        ),
    }
}

fn tool_input<'a>(
    items: &'a [AgentFrameItem],
    view: &ToolCallView,
) -> Option<&'a serde_json::Value> {
    match items.get(view.request_index)? {
        AgentFrameItem::ToolCall(call) if call.occurrence_id == view.occurrence_id => {
            Some(&call.input)
        }
        _ => None,
    }
}

fn permission_reason(items: &[AgentFrameItem], identity: &ToolCallIdentity) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        AgentFrameItem::Permission(permission) if &permission.identity == identity => {
            Some(permission.reason.clone())
        }
        _ => None,
    })
}

/// A finished call's body; an unfinished one shows only a proposed edit or
/// write.
fn body(items: &[AgentFrameItem], view: &ToolCallView) -> Option<CallBody> {
    if !view.finished() && !matches!(view.tool_id.as_str(), "fs.edit" | "fs.write") {
        return None;
    }
    let body = match tool_call_body(items, view)? {
        ToolCallBody::Diff { lines, omitted } => {
            let mut lines: Vec<DiffLine> = lines
                .into_iter()
                .map(|line| DiffLine {
                    kind: match line.kind {
                        DiffLineKind::Context => DiffKind::Context,
                        DiffLineKind::Added => DiffKind::Added,
                        DiffLineKind::Removed => DiffKind::Removed,
                    },
                    text: line.text,
                })
                .collect();
            if omitted > 0 {
                lines.push(DiffLine {
                    kind: DiffKind::Context,
                    text: chrome::more_lines(omitted),
                });
            }
            CallBody::Diff(lines)
        }
        ToolCallBody::ContentPreview { lines, omitted, .. }
        | ToolCallBody::Command { lines, omitted, .. }
        | ToolCallBody::Raw { lines, omitted } => CallBody::Output { lines, omitted },
        ToolCallBody::Summary(text) => CallBody::Text(text),
    };
    let empty = match &body {
        CallBody::Diff(lines) => lines.is_empty(),
        CallBody::Output { lines, omitted } => lines.is_empty() && *omitted == 0,
        CallBody::Text(text) => text.is_empty(),
    };
    (!empty).then_some(body)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use horizon_acp::{MemoryFieldUpdate, MemoryOp};
    use serde_json::json;

    use super::super::super::model::{
        AgentFrameItem, HistoryCleared, Message as FrameMessage, ModelSelection,
        ProviderRateLimited, SessionPhase,
    };
    use super::super::super::turns::test_support::{
        approval_requested, assistant_delta, edit_result, reasoning_delta, tool_finished,
        tool_requested, user_message,
    };
    use super::*;

    fn turn_end(reason: TurnEndReason) -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason,
            model: None,
            elapsed: Duration::from_secs(48),
        }
    }

    fn model(phase: SessionPhase, items: Vec<AgentFrameItem>) -> AgentModel {
        let mut model = AgentModel::default();
        model.frame.phase = Some(phase);
        model.frame.items = items;
        model
    }

    /// A fresh adaptation: nothing settled from before.
    fn adapt(model: &AgentModel, unreachable: bool, now_ms: u64) -> Adapted {
        super::adapt(model, unreachable, now_ms, &mut SettledUnits::default())
    }

    /// One asked-and-answered turn's items: the ask, a command that ran,
    /// and the answer.
    fn exchange(turn: usize, items: &mut Vec<AgentFrameItem>) {
        items.push(user_message(&format!("ask {turn}")));
        items.push(tool_finished(
            &format!("call-{turn}"),
            "bash",
            json!({"command": format!("ls {turn}")}),
            bash_output(&format!("out {turn}\n"), 0),
        ));
        items.push(assistant_delta(&format!("done {turn}")));
    }

    fn turns(transcript: &Transcript) -> Vec<&Turn> {
        transcript
            .units
            .iter()
            .filter_map(|unit| match unit.as_ref() {
                Unit::Turn(turn) => Some(turn),
                _ => None,
            })
            .collect()
    }

    /// The units as owned values, for comparison against literals.
    fn plain(transcript: &Transcript) -> Vec<Unit> {
        transcript
            .units
            .iter()
            .map(|unit| unit.as_ref().clone())
            .collect()
    }

    fn receipts(turn: &Turn) -> Vec<&Receipt> {
        turn.parts
            .iter()
            .filter_map(|part| match part {
                TurnPart::Receipt(receipt) => Some(receipt),
                _ => None,
            })
            .collect()
    }

    fn bash_output(output: &str, exit_code: i64) -> serde_json::Value {
        json!({"exit_code": exit_code, "output": output, "termination": "exited", "output_file": null, "truncated": false})
    }

    #[test]
    fn a_finished_exchange_is_a_message_and_one_done_turn() {
        let adapted = adapt(
            &model(
                SessionPhase::Idle,
                vec![
                    user_message("fix it"),
                    tool_finished(
                        "r1",
                        "fs.read",
                        json!({"path": "src/a.rs"}),
                        json!({"content": "x", "start_line": 1, "end_line": 1, "total_lines": 1}),
                    ),
                    assistant_delta("done"),
                    turn_end(TurnEndReason::Completed),
                ],
            ),
            false,
            0,
        );
        let transcript = &adapted.transcript;
        assert_eq!(
            *transcript.units[0],
            Unit::User(Message {
                label: Label::You,
                text: "fix it".to_string()
            })
        );
        let turns = turns(transcript);
        assert_eq!(turns.len(), 1);
        let turn = turns[0];
        assert_eq!(turn.state, TurnState::Done);
        assert_eq!(turn.elapsed.as_deref(), Some("48s"));
        let receipt = receipts(turn)[0];
        assert_eq!(receipt.summary, "Read 1 file");
        assert!(!receipt.running && !receipt.failed);
        assert_eq!(receipt.calls[0].title(), "Read src/a.rs");
        assert_eq!(receipt.calls[0].status, CallStatus::Done);
        assert_eq!(turn.parts[1], TurnPart::Text("done".to_string()));
        assert_eq!(transcript.status, Status::Idle);
        assert_eq!(transcript.composer, ComposerMode::Idle);
        assert!(adapted.approvals.is_empty());
    }

    #[test]
    fn a_message_after_the_first_output_starts_a_new_card() {
        let adapted = adapt(
            &model(
                SessionPhase::Idle,
                vec![
                    assistant_delta("initialized"),
                    user_message("q"),
                    assistant_delta("a"),
                    turn_end(TurnEndReason::Completed),
                ],
            ),
            false,
            0,
        );
        assert_eq!(
            plain(&adapted.transcript),
            vec![
                Unit::Turn(Turn {
                    state: TurnState::Done,
                    parts: vec![TurnPart::Text("initialized".to_string())],
                    elapsed: None,
                }),
                Unit::User(Message {
                    label: Label::You,
                    text: "q".to_string(),
                }),
                Unit::Turn(Turn {
                    state: TurnState::Done,
                    parts: vec![TurnPart::Text("a".to_string())],
                    elapsed: Some("48s".to_string()),
                }),
            ]
        );
    }

    #[test]
    fn the_summary_counts_each_kind_in_the_adopted_wording() {
        let items = vec![
            user_message("go"),
            tool_finished(
                "e1",
                "fs.edit",
                json!({"edits": [{"path": "a.rs", "old_string": "a", "new_string": "b"}]}),
                edit_result("a.rs"),
            ),
            tool_finished(
                "e2",
                "fs.edit",
                json!({"edits": [{"path": "b.rs", "old_string": "a", "new_string": "b"}]}),
                edit_result("b.rs"),
            ),
            tool_finished(
                "b1",
                "bash",
                json!({"command": "cargo test\n--quiet"}),
                bash_output("ok", 0),
            ),
            tool_finished(
                "b2",
                "bash",
                json!({"command": "cargo clippy"}),
                json!({"is_error": true}),
            ),
            tool_finished(
                "g1",
                "fs.grep",
                json!({"base_path": ".", "pattern": "font_size"}),
                json!({"returned_count": 3, "matches": [], "truncated": false}),
            ),
            assistant_delta("summary"),
            turn_end(TurnEndReason::Completed),
        ];
        let adapted = adapt(&model(SessionPhase::Idle, items), false, 0);
        let turn = turns(&adapted.transcript)[0];
        let receipt = receipts(turn)[0];
        assert_eq!(
            receipt.summary,
            "Edited 2 files · Ran 2 commands · 1 search"
        );
        assert!(receipt.failed);
        let titles: Vec<(String, CallStatus)> = receipt
            .calls
            .iter()
            .map(|call| (call.title(), call.status))
            .collect();
        assert_eq!(
            titles,
            vec![
                ("Edit a.rs".to_string(), CallStatus::Done),
                ("Edit b.rs".to_string(), CallStatus::Done),
                ("Run cargo test".to_string(), CallStatus::Done),
                ("Run cargo clippy".to_string(), CallStatus::Failed),
                ("Search font_size".to_string(), CallStatus::Done),
            ]
        );
        assert!(matches!(
            &receipt.calls[0].body,
            Some(CallBody::Diff(lines)) if lines.iter().any(|line| line.kind == DiffKind::Added)
        ));
        assert!(matches!(
            &receipt.calls[2].body,
            Some(CallBody::Output { lines, omitted: 0 }) if lines == &["ok".to_string()]
        ));
        assert!(matches!(&receipt.calls[4].body, Some(CallBody::Text(_))));
    }

    #[test]
    fn consecutive_receipts_merge_into_the_first() {
        let run = |id: &str, command: &str| {
            tool_finished(
                id,
                "bash",
                json!({ "command": command }),
                bash_output("ok", 0),
            )
        };
        let items = vec![
            user_message("go"),
            run("b1", "ls"),
            run("b2", "pwd"),
            assistant_delta(""),
            tool_finished(
                "g1",
                "fs.grep",
                json!({"base_path": ".", "pattern": "x"}),
                json!({"returned_count": 1, "matches": [], "truncated": false}),
            ),
            AgentFrameItem::HistoryCleared(HistoryCleared {
                cleared_occurrence_ids: vec![],
                recovered_chars: 0,
            }),
            run("b3", "make"),
            assistant_delta("between"),
            run("b4", "date"),
            assistant_delta("after"),
            turn_end(TurnEndReason::Completed),
        ];
        let adapted = adapt(&model(SessionPhase::Idle, items), false, 0);
        let turn = turns(&adapted.transcript)[0];
        let receipts = receipts(turn);
        assert_eq!(receipts.len(), 2);
        let merged = receipts[0];
        assert_eq!(merged.key, 1);
        assert_eq!(merged.summary, "Ran 3 commands · 1 search");
        let titles: Vec<String> = merged.calls.iter().map(Call::title).collect();
        assert_eq!(
            titles,
            ["Run ls", "Run pwd", "Search x", "Run make"].map(str::to_string)
        );
        assert!(!merged.running && !merged.failed);
        assert_eq!(receipts[1].key, 8);
        assert_eq!(receipts[1].summary, "Ran 1 command");
        assert!(matches!(turn.parts[0], TurnPart::Receipt(_)));
        assert_eq!(turn.parts[1], TurnPart::Text("between".to_string()));
        assert!(matches!(turn.parts[2], TurnPart::Receipt(_)));
        assert_eq!(turn.parts[3], TurnPart::Text("after".to_string()));
    }

    #[test]
    fn a_pending_approval_rides_its_call_and_the_composer() {
        let items = vec![
            user_message("write it"),
            tool_requested(
                "w1",
                "fs.write",
                json!({"path": "~/.config/x.toml", "content": "a = 1"}),
            ),
            approval_requested("w1"),
        ];
        let adapted = adapt(&model(SessionPhase::RequiresAction, items), false, 0);
        let transcript = &adapted.transcript;
        assert_eq!(transcript.status, Status::WaitingForApproval);
        let turn = turns(transcript)[0];
        assert_eq!(turn.state, TurnState::Running);
        let receipt = receipts(turn)[0];
        assert!(receipt.running);
        let call = &receipt.calls[0];
        assert_eq!(call.title(), "Write ~/.config/x.toml");
        assert_eq!(call.status, CallStatus::Running);
        let approval = call
            .approval
            .as_ref()
            .expect("the call carries its approval");
        assert_eq!(approval.reason, "writes a file");
        assert_eq!(
            transcript.composer,
            ComposerMode::Approval {
                id: approval.id.clone()
            }
        );
        let identity = &adapted.approvals[&approval.id];
        assert_eq!(identity.call_id, "w1");
        assert_eq!(identity.occurrence_id, "w1");
        assert_eq!(turn.parts.last(), Some(&TurnPart::Thinking));
    }

    #[test]
    fn a_running_turn_thinks_until_text_arrives_or_while_a_thought_streams() {
        let thinking = adapt(
            &model(
                SessionPhase::Running,
                vec![user_message("q"), reasoning_delta("hmm")],
            ),
            false,
            0,
        );
        let turn = turns(&thinking.transcript)[0];
        assert_eq!(turn.parts, vec![TurnPart::Thinking]);
        assert_eq!(thinking.transcript.composer, ComposerMode::Running);

        let answering = adapt(
            &model(
                SessionPhase::Running,
                vec![
                    user_message("q"),
                    reasoning_delta("hmm"),
                    assistant_delta("a"),
                ],
            ),
            false,
            0,
        );
        let turn = turns(&answering.transcript)[0];
        assert_eq!(turn.parts, vec![TurnPart::Text("a".to_string())]);

        let waiting = adapt(
            &model(SessionPhase::Running, vec![user_message("q")]),
            false,
            0,
        );
        let turn = turns(&waiting.transcript)[0];
        assert_eq!(turn.state, TurnState::Running);
        assert_eq!(turn.parts, vec![TurnPart::Thinking]);
    }

    #[test]
    fn turn_end_reasons_map_to_turn_states() {
        for (reason, state) in [
            (TurnEndReason::Completed, TurnState::Done),
            (TurnEndReason::Cancelled, TurnState::Stopped),
            (TurnEndReason::Failed, TurnState::Failed),
            (TurnEndReason::HaltedByIterationCap, TurnState::Failed),
            (TurnEndReason::HaltedByDoomLoop, TurnState::Failed),
        ] {
            let adapted = adapt(
                &model(
                    SessionPhase::Idle,
                    vec![user_message("q"), assistant_delta("a"), turn_end(reason)],
                ),
                false,
                0,
            );
            assert_eq!(turns(&adapted.transcript)[0].state, state, "{reason:?}");
        }
    }

    #[test]
    fn system_items_read_as_notices_after_the_turn() {
        let items = vec![
            user_message("q"),
            AgentFrameItem::ProviderRateLimited(ProviderRateLimited {
                status: Some(429),
                attempt: 2,
                backoff_ms: 1500,
            }),
            assistant_delta("a"),
            AgentFrameItem::HistoryCleared(HistoryCleared {
                cleared_occurrence_ids: (0..12).map(|i| i.to_string()).collect(),
                recovered_chars: 31_204,
            }),
            AgentFrameItem::MemoryDigest(MemoryDigest {
                updates: vec![MemoryFieldUpdate {
                    field: MemoryField::NextStep,
                    op: MemoryOp::Set,
                    content: "x".to_string(),
                }],
                folded_log_range: None,
                no_update_reason: None,
            }),
            AgentFrameItem::MemoryCheckpointMissed,
            AgentFrameItem::Error("boom".to_string()),
            turn_end(TurnEndReason::Failed),
            AgentFrameItem::Exited("gone".to_string()),
        ];
        let adapted = adapt(&model(SessionPhase::Terminated, items), false, 0);
        let notices: Vec<(Tone, String)> = adapted
            .transcript
            .units
            .iter()
            .filter_map(|unit| match unit.as_ref() {
                Unit::Notice(notice) => Some((notice.tone, notice.text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            notices,
            vec![
                (
                    Tone::Warning,
                    "Provider rate limited (attempt 2, retrying in 2s)".to_string()
                ),
                (
                    Tone::Muted,
                    "History compacted: 12 tool outputs (31,204 chars) reclaimed".to_string()
                ),
                (Tone::Muted, "Memory updated: next step".to_string()),
                (Tone::Warning, "Memory checkpoint missed".to_string()),
                (Tone::Danger, "boom".to_string()),
                (Tone::Danger, "gone".to_string()),
            ]
        );
        assert!(matches!(*adapted.transcript.units[1], Unit::Notice(_)));
        assert!(matches!(*adapted.transcript.units[2], Unit::Turn(_)));
        assert_eq!(adapted.transcript.status, Status::Terminated);
    }

    #[test]
    fn task_and_continue_messages_carry_their_labels() {
        let message = |role, text: &str| {
            AgentFrameItem::Message(FrameMessage {
                id: text.to_string(),
                role,
                text: text.to_string(),
            })
        };
        let adapted = adapt(
            &model(
                SessionPhase::Idle,
                vec![
                    message(MessageRole::TaskNotification, "task done"),
                    assistant_delta("ok"),
                    turn_end(TurnEndReason::Completed),
                    message(MessageRole::AutoContinue, "continue"),
                ],
            ),
            false,
            0,
        );
        let labels: Vec<Label> = adapted
            .transcript
            .units
            .iter()
            .filter_map(|unit| match unit.as_ref() {
                Unit::User(message) => Some(message.label),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec![Label::Task, Label::Continue]);
    }

    #[test]
    fn ended_turns_are_worded_once_and_kept_across_adaptations() {
        let mut items = Vec::new();
        for turn in 0..3 {
            exchange(turn, &mut items);
            items.push(turn_end(TurnEndReason::Completed));
        }
        exchange(3, &mut items);
        let mut model = model(SessionPhase::Running, items);
        let mut settled = SettledUnits::default();

        let first = super::adapt(&model, false, 0, &mut settled);
        assert_eq!(first, adapt(&model, false, 0));
        assert_eq!(settled.len(), 3, "one entry per ended turn");

        // The open turn grows: the ended turns are not worded again, and
        // the result still matches a fresh adaptation.
        model.frame.items.push(assistant_delta("and more"));
        let again = super::adapt(&model, false, 0, &mut settled);
        assert_eq!(again, adapt(&model, false, 0));
        assert_eq!(settled.len(), 3);
        assert_ne!(first.transcript.units, again.transcript.units);

        // The open turn ends: it joins the settled ones.
        model.frame.items.push(turn_end(TurnEndReason::Completed));
        model.frame.phase = Some(SessionPhase::Idle);
        let ended = super::adapt(&model, false, 0, &mut settled);
        assert_eq!(ended, adapt(&model, false, 0));
        assert_eq!(settled.len(), 4);
    }

    /// Not a check: a measurement of what one adaptation of a long session
    /// costs, fresh against kept. Run it by hand with `--run-ignored all`
    /// and `--nocapture`.
    #[test]
    #[ignore = "a timing, printed for the reader; not a check"]
    fn adaptation_cost_of_a_long_session() {
        let mut items = Vec::new();
        for turn in 0..400 {
            items.push(user_message(&format!("ask {turn}")));
            for call in 0..3 {
                let id = format!("call-{turn}-{call}");
                items.push(tool_finished(
                    &id,
                    "fs.edit",
                    json!({"path": "/repo/src/lib.rs", "edits": [{"old_string": "fn old() {}\n".repeat(20), "new_string": "fn new() {}\n".repeat(20)}]}),
                    edit_result("/repo/src/lib.rs"),
                ));
                items.push(tool_finished(
                    &format!("{id}-run"),
                    "bash",
                    json!({"command": "cargo test"}),
                    bash_output(&"test result: ok\n".repeat(40), 0),
                ));
            }
            items.push(assistant_delta(&"Done with this step. ".repeat(30)));
            items.push(turn_end(TurnEndReason::Completed));
        }
        items.push(user_message("one more"));
        items.push(assistant_delta("Working"));
        let model = model(SessionPhase::Running, items);

        let rounds = 20;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            adapt(&model, false, 0);
        }
        let fresh = started.elapsed() / rounds;

        let mut settled = SettledUnits::default();
        super::adapt(&model, false, 0, &mut settled);
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            super::adapt(&model, false, 0, &mut settled);
        }
        let kept = started.elapsed() / rounds;
        println!(
            "{} items, {} units: fresh {fresh:?} per adaptation, with kept ended turns {kept:?}",
            model.frame.items.len(),
            adapt(&model, false, 0).transcript.units.len()
        );
    }

    #[test]
    fn a_moved_settled_revision_rewords_the_ended_turns() {
        let mut items = Vec::new();
        exchange(0, &mut items);
        items.push(turn_end(TurnEndReason::Completed));
        let mut model = model(SessionPhase::Idle, items);
        let mut settled = SettledUnits::default();
        let before = super::adapt(&model, false, 0, &mut settled);

        // A settled item changes without the revision moving: the kept
        // wording is what shows, which is exactly why the fold moves the
        // revision on such a change.
        if let AgentFrameItem::Message(message) = &mut model.frame.items[0] {
            message.text = "edited ask".to_string();
        }
        assert_eq!(
            super::adapt(&model, false, 0, &mut settled),
            before,
            "an unmoved revision keeps the ended turn's wording"
        );
        model.frame.settled_revision += 1;
        let after = super::adapt(&model, false, 0, &mut settled);
        assert_eq!(after, adapt(&model, false, 0));
        assert_ne!(after, before);
        assert_eq!(settled.len(), 1);
    }

    #[test]
    fn the_model_tasks_and_reachability_come_from_the_session() {
        let mut session = model(SessionPhase::Idle, vec![]);
        session.selection = Some(ModelSelection {
            provider: "openai".to_string(),
            model: "gpt-x".to_string(),
        });
        session.tasks.push(TaskProgressNotification {
            session_id: horizon_wire::SessionId::new(),
            task_session_id: horizon_wire::SessionId::new(),
            description: "cargo build".to_string(),
            state: horizon_acp::TaskProgressState::Running,
            activity: Some("compiling".to_string()),
            started_at_epoch_ms: 1_000,
        });
        let adapted = adapt(&session, true, 64_000);
        assert_eq!(adapted.transcript.model, "gpt-x");
        assert_eq!(adapted.transcript.status, Status::Unreachable);
        assert_eq!(
            adapted.transcript.tasks,
            vec![Task {
                description: "cargo build".to_string(),
                elapsed: "1m 03s".to_string(),
                activity: "compiling".to_string(),
            }]
        );
        assert_eq!(adapt(&AgentModel::default(), false, 0).transcript.model, "");
    }
}
