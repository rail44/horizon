use super::super::conversation::{ConversationHistory, Prompt};
use super::*;
use crate::contract::ProviderEvent;
use crate::contract::{ConversationInputKind, OccurrenceId, ToolCallId, ToolCallIdentity};
use crossbeam_channel::{unbounded, Sender};
use rig_core::completion::message::{ToolCall, ToolFunction};

fn id(value: &str) -> OccurrenceId {
    OccurrenceId(value.into())
}
fn channel() -> Sender<ProviderEvent> {
    unbounded().0
}
fn input(history: &mut ConversationHistory, kind: ConversationInputKind, text: &str) {
    history
        .append_prompt(Prompt::input(kind, text), &channel())
        .unwrap();
}
fn round(
    history: &mut ConversationHistory,
    provider: &str,
    occurrence: &str,
    path: &str,
    size: usize,
) {
    let identity = ToolCallIdentity {
        call_id: ToolCallId(provider.into()),
        occurrence_id: id(occurrence),
    };
    let message = Message::Assistant {
        id: Some(format!("message-{occurrence}")),
        content: vec![rig_core::completion::AssistantContent::ToolCall(
            ToolCall::new(
                rig_core::message::ToolCallId::new_or_mint(provider),
                ToolFunction::new("fs.read".into(), serde_json::json!({"path":path})),
            ),
        )],
    };
    history
        .record_response(
            occurrence.into(),
            &message,
            vec![identity.clone()],
            &channel(),
        )
        .unwrap();
    history
        .append_result(
            &identity.result(serde_json::json!({"content":"x".repeat(size)})),
            "fs.read",
        )
        .unwrap();
}
fn history() -> ConversationHistory {
    let mut history = ConversationHistory::default();
    history.open_turn(&channel());
    input(
        &mut history,
        ConversationInputKind::User,
        "audit these files",
    );
    for index in 0..40 {
        round(
            &mut history,
            &format!("call-{index}"),
            &format!("occ-{index}"),
            &format!("file-{index}.rs"),
            16_384,
        );
    }
    history
}
fn body(message: &Message) -> String {
    let Message::User { content } = message else {
        panic!("tool result")
    };
    let UserContent::ToolResult(result) = &content[0] else {
        panic!("tool result")
    };
    let ToolResultContent::Text(text) = &result.content[0] else {
        panic!("text")
    };
    text.text.clone()
}
fn memory() -> MemoryDocument {
    MemoryDocument {
        goal: "Keep the task".into(),
        ..Default::default()
    }
}

#[test]
fn measured_threshold_and_unknown_window_control_clearing() {
    for (window, usage, expected) in [
        (None, u64::MAX, false),
        (Some(200_000), 119_999, false),
        (Some(200_000), 120_000, true),
    ] {
        let mut state = ClearingState::new(window, 60);
        state.record_input_tokens(usage);
        assert_eq!(state.run_pass(&history()).is_some(), expected);
    }
    let mut state = ClearingState::new(Some(200_000), 60);
    state.record_input_tokens(100_000);
    state.adopt_window(Some(150_000));
    assert!(state.run_pass(&history()).is_some());
    state.adopt_window(None);
    assert!(state.run_pass(&history()).is_none());
}
#[test]
fn floor_tail_and_current_batch_prevent_small_or_unsafe_passes() {
    let mut small = ConversationHistory::default();
    round(&mut small, "one", "one", "one.rs", 100);
    let mut state = ClearingState::new(Some(1), 1);
    state.record_input_tokens(1);
    assert!(state.run_pass(&small).is_none());
    let history = history();
    let plan = plan_clearing_pass(&history, &ClearedResults::default());
    assert!(plan.cleared_occurrence_ids.contains(&id("occ-0")));
    for index in 36..40 {
        assert!(!plan
            .cleared_occurrence_ids
            .contains(&id(&format!("occ-{index}"))));
    }
    assert_eq!(plan.cleared_occurrence_ids.len(), 36);
    assert!(plan.recovered_chars >= 36 * 16_384);
}
#[test]
fn a_reused_id_keeps_the_original_calls_placeholder_description() {
    let mut history = ConversationHistory::default();
    round(&mut history, "same", "old", "old.rs", 100);
    round(&mut history, "same", "new", "new.rs", 100);
    let projected = history_for_provider_request(
        &history,
        &ClearedResults::from_occurrences([id("old")]),
        None,
        None,
    );
    assert!(body(&projected[1]).contains("old.rs"));
    assert!(!body(&projected[1]).contains("new.rs"));
    assert!(!body(&projected[3]).contains("cleared"));
}
#[test]
fn a_later_pass_can_clear_an_aged_result_whose_provider_id_was_reused() {
    let mut history = history();
    let mut state = ClearingState::new(Some(1), 1);
    state.record_input_tokens(1);
    let first = state.run_pass(&history).unwrap();
    round(&mut history, "call-0", "later", "later.rs", 70_000);
    for index in 40..45 {
        round(
            &mut history,
            &format!("call-{index}"),
            &format!("occ-{index}"),
            "tail.rs",
            20_000,
        );
    }
    let second = state.run_pass(&history).unwrap();
    assert!(second.cleared_occurrence_ids.contains(&id("later")));
    assert!(second
        .cleared_occurrence_ids
        .iter()
        .all(|id| !first.cleared_occurrence_ids.contains(id)));
    assert!(state.run_pass(&history).is_none());
}
#[test]
fn projection_preserves_canonical_messages_and_freezes_exact_results_across_replay() {
    let history = history();
    let before = history.messages();
    let mut state = ClearingState::new(Some(1), 1);
    state.record_input_tokens(1);
    let pass = state.run_pass(&history).unwrap();
    let first = history_for_provider_request(&history, state.cleared(), None, None);
    assert_eq!(before, history.messages());
    assert_eq!(first[0], before[0]);
    let mut resumed = ClearingState::disabled();
    resumed.seed_cleared(cleared_occurrence_ids_from_events(&[
        Event::HistoryCleared(pass.clone()),
    ]));
    resumed.seed_cleared(pass.cleared_occurrence_ids);
    assert_eq!(
        first,
        history_for_provider_request(&history, resumed.cleared(), None, None)
    );
}
#[test]
fn internal_inputs_and_proposals_do_not_move_the_memory_turn_boundary() {
    let mut history = history();
    history.open_turn(&channel());
    input(
        &mut history,
        ConversationInputKind::User,
        "current owner request",
    );
    round(&mut history, "call", "now", "now.rs", 100);
    input(
        &mut history,
        ConversationInputKind::Notification,
        "child completed",
    );
    input(
        &mut history,
        ConversationInputKind::Continuation,
        "continue after truncation",
    );
    let proposal = (history.turn_start(), Message::user("proposals"));
    let projected = history_for_provider_request(
        &history,
        &ClearedResults::default(),
        Some(&memory()),
        Some(&proposal),
    );
    let text = serde_json::to_string(&projected).unwrap();
    assert!(text.contains("Keep the task"));
    assert!(text.contains("current owner request"));
    assert!(text.contains("child completed"));
    assert!(text.contains("continue after truncation"));
    assert!(text.contains("proposals"));
    assert!(!text.contains("file-0.rs"));
    assert_eq!(projected.len(), 7);
}
#[test]
fn empty_memory_keeps_history_and_unicode_descriptions_remain_bounded() {
    let history = history();
    assert_eq!(
        history.messages(),
        history_for_provider_request(
            &history,
            &ClearedResults::default(),
            Some(&MemoryDocument::default()),
            None
        )
    );
    let argument = key_argument(&serde_json::json!({"path":"界".repeat(200)})).unwrap();
    assert!(argument.ends_with("…\""));
    assert!(argument.chars().count() < 140);
}

#[test]
fn clearing_an_old_task_report_leaves_task_output_able_to_re_fetch_it() {
    use crate::contract::{SessionId, ToolCallRequest};
    let requester = SessionId::new();
    let child = SessionId::new();
    let report = "the child's full findings".repeat(200);
    crate::tools::explore::register_finished_child_for_test(
        requester,
        child,
        "audit the parser",
        serde_json::json!({"report":report,"is_error":false}),
    );
    let mut history = ConversationHistory::default();
    round(
        &mut history,
        "task-1",
        "task-occurrence",
        "original-report",
        report.len(),
    );
    let projected = history_for_provider_request(
        &history,
        &ClearedResults::from_occurrences([id("task-occurrence")]),
        None,
        None,
    );
    assert!(body(&projected[1]).contains("cleared old tool result"));
    let fetch = crate::tools::explore::output(
        requester,
        &ToolCallRequest {
            call_id: ToolCallId("fetch".into()),
            occurrence_id: id("fetch"),
            tool_id: crate::tools::TASK_OUTPUT_TOOL_ID.into(),
            input: serde_json::json!({"session_id":child.as_uuid()}).into(),
        },
        &crate::tools::input::TaskOutput {
            session_id: child.as_uuid(),
        },
    );
    assert_eq!(fetch.output.to_json()["report"], report);
}
