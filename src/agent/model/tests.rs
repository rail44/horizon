use agent_client_protocol::schema::{v2, MaybeUndefined};
use horizon_acp::{
    write_horizon_meta, ApprovalKind, ApprovalMeta, MessageMeta, ModelOptionMeta,
    ProviderRequestEvent, SessionEventNotification, SessionId, TaskProgressNotification,
    TaskProgressState, ToolCallMeta, ToolCallProgressEvent, ToolOutcome, MODEL_CONFIG_ID,
    STOP_REASON_DOOM_LOOP, STOP_REASON_FAILED,
};
use horizon_agent::transcript::ApprovalState;
use serde_json::json;

use super::*;

fn apply(model: &mut AgentModel, update: v2::SessionUpdate) {
    model.apply(AgentEvent::Update(Box::new(update)));
}

fn agent_chunk(id: &str, text: &str) -> v2::SessionUpdate {
    v2::SessionUpdate::AgentMessageChunk(v2::ContentChunk::new(text.into(), id))
}

fn user_message(id: &str, text: &str) -> v2::SessionUpdate {
    v2::SessionUpdate::UserMessage(v2::UserMessage::new(id).content(vec![text.into()]))
}

fn running() -> v2::SessionUpdate {
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::Running(v2::RunningStateUpdate::new()))
}

fn requires_action() -> v2::SessionUpdate {
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::RequiresAction(
        v2::RequiresActionStateUpdate::new(),
    ))
}

fn idle(reason: Option<v2::StopReason>) -> v2::SessionUpdate {
    let mut idle = v2::IdleStateUpdate::new();
    idle.stop_reason = reason;
    v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(idle))
}

fn tool_update(
    occurrence: &str,
    status: Option<v2::ToolCallStatus>,
    meta: Option<ToolCallMeta>,
) -> v2::ToolCallUpdate {
    let mut update = v2::ToolCallUpdate::new(occurrence);
    if let Some(status) = status {
        update.status = MaybeUndefined::Value(status);
    }
    if let Some(meta) = meta {
        let mut encoded = None;
        write_horizon_meta(&mut encoded, &meta).unwrap();
        update.meta = MaybeUndefined::Value(encoded.unwrap());
    }
    update
}

fn meta(call_id: &str, tool_id: &str, outcome: Option<ToolOutcome>) -> ToolCallMeta {
    ToolCallMeta {
        call_id: call_id.into(),
        tool_id: tool_id.into(),
        outcome,
        auto_approved: None,
        policy_tier: None,
    }
}

fn permission(call_id: &str, occurrence: &str) -> Permission {
    let mut request_meta = None;
    write_horizon_meta(
        &mut request_meta,
        &ApprovalMeta {
            call_id: call_id.into(),
            occurrence_id: occurrence.into(),
            kind: ApprovalKind::Standard,
        },
    )
    .unwrap();
    let mut request = v2::RequestPermissionRequest::new(
        "session",
        "Run bash",
        vec![
            v2::PermissionOption::new(
                horizon_acp::PERMISSION_OPTION_APPROVE,
                "Approve",
                v2::PermissionOptionKind::AllowOnce,
            ),
            v2::PermissionOption::new(
                horizon_acp::PERMISSION_OPTION_DENY,
                "Deny",
                v2::PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    request.meta = request_meta;
    Permission::from_request(&request).expect("the request carries ApprovalMeta")
}

fn identity(call_id: &str, occurrence: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        call_id: call_id.into(),
        occurrence_id: occurrence.into(),
    }
}

fn texts(model: &AgentModel) -> Vec<(MessageRole, String)> {
    model
        .frame
        .items
        .iter()
        .filter_map(|item| match item {
            AgentFrameItem::Message(message) => Some((message.role, message.text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn chunks_append_per_message_id_and_a_snapshot_replaces() {
    let mut model = AgentModel::default();
    apply(&mut model, user_message("u1", "hello"));
    apply(&mut model, agent_chunk("a1", "Hi "));
    apply(&mut model, agent_chunk("a1", "there"));
    apply(&mut model, agent_chunk("a2", "second"));
    assert_eq!(
        texts(&model),
        [
            (MessageRole::User, "hello".to_string()),
            (MessageRole::Assistant, "Hi there".to_string()),
            (MessageRole::Assistant, "second".to_string()),
        ]
    );

    apply(
        &mut model,
        v2::SessionUpdate::AgentMessage(v2::AgentMessage::new("a1").content(vec!["Hello!".into()])),
    );
    // A metadata-only patch keeps the content; `null` clears it.
    apply(
        &mut model,
        v2::SessionUpdate::AgentMessage(v2::AgentMessage::new("a2")),
    );
    assert_eq!(texts(&model)[1].1, "Hello!");
    assert_eq!(texts(&model)[2].1, "second");
    apply(
        &mut model,
        v2::SessionUpdate::AgentMessage(v2::AgentMessage::new("a2").content(MaybeUndefined::Null)),
    );
    assert_eq!(texts(&model)[2].1, "");
    assert_eq!(model.frame.items.len(), 3, "patches never add items");
}

#[test]
fn replaying_the_same_snapshot_is_idempotent() {
    let mut model = AgentModel::default();
    for _ in 0..2 {
        apply(&mut model, user_message("u1", "hello"));
        apply(
            &mut model,
            v2::SessionUpdate::AgentMessage(v2::AgentMessage::new("a1").content(vec!["hi".into()])),
        );
    }
    assert_eq!(model.frame.items.len(), 2);
}

#[test]
fn message_meta_carries_system_authored_roles() {
    let mut model = AgentModel::default();
    let mut meta = None;
    write_horizon_meta(
        &mut meta,
        &MessageMeta {
            role: horizon_acp::MessageRole::TaskNotification,
        },
    )
    .unwrap();
    apply(
        &mut model,
        v2::SessionUpdate::UserMessage(
            v2::UserMessage::new("t1")
                .content(vec!["task done".into()])
                .meta(meta.unwrap()),
        ),
    );
    assert_eq!(
        texts(&model),
        [(MessageRole::TaskNotification, "task done".to_string())]
    );
}

#[test]
fn thoughts_fold_by_message_id() {
    let mut model = AgentModel::default();
    apply(
        &mut model,
        v2::SessionUpdate::AgentThoughtChunk(v2::ContentChunk::new("hmm ".into(), "r1")),
    );
    apply(
        &mut model,
        v2::SessionUpdate::AgentThoughtChunk(v2::ContentChunk::new("ok".into(), "r1")),
    );
    assert!(matches!(
        model.frame.items.as_slice(),
        [AgentFrameItem::Thought(Thought { text, .. })] if text == "hmm ok"
    ));
}

#[test]
fn a_tool_call_upserts_by_occurrence_id_through_its_lifecycle() {
    let mut model = AgentModel::default();
    let mut request = tool_update(
        "occ-1",
        Some(v2::ToolCallStatus::Pending),
        Some(meta("call-1", "bash", None)),
    );
    request.raw_input = MaybeUndefined::Value(json!({"command": "echo hi"}));
    apply(&mut model, v2::SessionUpdate::ToolCallUpdate(request));
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].identity(), identity("call-1", "occ-1"));
    assert_eq!(views[0].tool_id, "bash");
    assert!(!views[0].finished());

    apply(
        &mut model,
        v2::SessionUpdate::ToolCallUpdate(tool_update(
            "occ-1",
            Some(v2::ToolCallStatus::InProgress),
            None,
        )),
    );
    apply(&mut model, running());
    assert_eq!(model.frame.state(), Some(SessionState::ToolRunning));

    let mut finished = tool_update(
        "occ-1",
        Some(v2::ToolCallStatus::Completed),
        Some(meta("call-1", "bash", Some(ToolOutcome::Succeeded))),
    );
    finished.raw_output = MaybeUndefined::Value(json!({
        "exit_code": 0, "output": "hi", "termination": "exited",
        "output_file": null, "truncated": false
    }));
    apply(&mut model, v2::SessionUpdate::ToolCallUpdate(finished));
    assert_eq!(model.frame.items.len(), 1, "one item per occurrence");
    let views = build_tool_call_views(&model.frame.items);
    assert!(views[0].is_success());
    assert_eq!(views[0].result_index, Some(0));
    assert_eq!(model.frame.state(), Some(SessionState::Running));

    // A retry under the same call id is its own occurrence; the old one
    // closes as superseded.
    apply(
        &mut model,
        v2::SessionUpdate::ToolCallUpdate(tool_update(
            "occ-2",
            Some(v2::ToolCallStatus::Pending),
            Some(meta("call-1", "bash", None)),
        )),
    );
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views.len(), 2);
    assert_eq!(views[1].identity(), identity("call-1", "occ-2"));
}

#[test]
fn status_alone_settles_the_outcome_when_meta_carries_none() {
    let mut model = AgentModel::default();
    apply(
        &mut model,
        v2::SessionUpdate::ToolCallUpdate(tool_update(
            "occ-1",
            Some(v2::ToolCallStatus::Cancelled),
            Some(meta("call-1", "fs.read", None)),
        )),
    );
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views[0].outcome, Some(ToolOutcome::Cancelled));
    assert_eq!(views[0].result_summary.as_deref(), Some("cancelled"));
}

#[test]
fn pending_approvals_are_the_unanswered_permission_requests_of_the_open_turn() {
    let mut model = AgentModel::default();
    apply(
        &mut model,
        v2::SessionUpdate::ToolCallUpdate(tool_update(
            "occ-1",
            Some(v2::ToolCallStatus::Pending),
            Some(meta("call-1", "bash", None)),
        )),
    );
    model.apply(AgentEvent::PermissionRequested(permission(
        "call-1", "occ-1",
    )));
    model.apply(AgentEvent::PermissionRequested(permission(
        "call-2", "occ-2",
    )));
    apply(&mut model, requires_action());
    assert_eq!(model.frame.state(), Some(SessionState::WaitingForApproval));
    assert_eq!(
        actionable_pending_approval_identities_in(&model.frame.items),
        [identity("call-1", "occ-1"), identity("call-2", "occ-2")]
    );
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views[0].approval, ApprovalState::Waiting);

    // A re-sent request for the same occurrence does not duplicate it.
    model.apply(AgentEvent::PermissionRequested(permission(
        "call-1", "occ-1",
    )));
    assert_eq!(
        actionable_pending_approval_identities_in(&model.frame.items).len(),
        2
    );

    model.apply(AgentEvent::PermissionResolved {
        identity: identity("call-1", "occ-1"),
        decision: PermissionDecision::Approved,
    });
    assert_eq!(
        actionable_pending_approval_identities_in(&model.frame.items),
        [identity("call-2", "occ-2")]
    );
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views[0].approval, ApprovalState::Approved);

    // A turn that ended leaves nothing actionable behind it.
    apply(&mut model, idle(Some(v2::StopReason::Cancelled)));
    assert!(actionable_pending_approval_identities_in(&model.frame.items).is_empty());
}

#[test]
fn a_denied_permission_reads_denied_before_the_result_arrives() {
    let mut model = AgentModel::default();
    apply(
        &mut model,
        v2::SessionUpdate::ToolCallUpdate(tool_update(
            "occ-1",
            None,
            Some(meta("call-1", "fs.edit", None)),
        )),
    );
    model.apply(AgentEvent::PermissionRequested(permission(
        "call-1", "occ-1",
    )));
    model.apply(AgentEvent::PermissionResolved {
        identity: identity("call-1", "occ-1"),
        decision: PermissionDecision::Denied,
    });
    let views = build_tool_call_views(&model.frame.items);
    assert_eq!(views[0].approval, ApprovalState::Denied);
}

#[test]
fn status_follows_state_updates_and_retains_the_stop_result() {
    let mut model = AgentModel::default();
    assert_eq!(model.frame.status(), None);
    apply(&mut model, idle(None));
    assert_eq!(model.frame.status(), Some(SessionStatus::WaitingForInput));
    assert!(
        model.frame.items.is_empty(),
        "idle without a reason ends no turn"
    );

    for (reason, expected) in [
        (v2::StopReason::EndTurn, SessionStatus::WaitingForInput),
        (v2::StopReason::Cancelled, SessionStatus::Cancelled),
        (v2::StopReason::MaxTurnRequests, SessionStatus::Paused),
        (
            v2::StopReason::Other(STOP_REASON_DOOM_LOOP.into()),
            SessionStatus::Paused,
        ),
        (
            v2::StopReason::Other(STOP_REASON_FAILED.into()),
            SessionStatus::Failed,
        ),
    ] {
        apply(&mut model, running());
        assert_eq!(model.frame.status(), Some(SessionStatus::Running));
        apply(&mut model, idle(Some(reason)));
        assert_eq!(model.frame.status(), Some(expected));
    }
    apply(&mut model, requires_action());
    assert_eq!(
        model.frame.status(),
        Some(SessionStatus::WaitingForApproval)
    );

    model.apply(AgentEvent::Session(SessionEventNotification::Exited {
        session_id: SessionId::new(),
        reason: "terminated".into(),
    }));
    assert_eq!(model.frame.status(), Some(SessionStatus::Terminated));
}

#[test]
fn a_turn_end_records_the_model_of_its_last_provider_request() {
    let mut model = AgentModel::default();
    apply(&mut model, user_message("u1", "go"));
    apply(&mut model, running());
    model.apply(AgentEvent::ProviderRequest(ProviderRequestEvent::Sent {
        model: "gpt-5".into(),
    }));
    apply(&mut model, idle(Some(v2::StopReason::EndTurn)));
    // A repeated idle for the same stop does not end a second turn.
    apply(&mut model, idle(Some(v2::StopReason::EndTurn)));
    let turns = group_into_turns(&model.frame.items);
    assert_eq!(turns.len(), 1);
    let end = turns[0].ended.as_ref().unwrap();
    assert_eq!(end.reason, TurnEndReason::Completed);
    assert_eq!(end.model.as_deref(), Some("gpt-5"));
    assert_eq!(latest_turn_model(&model.frame.items), Some("gpt-5"));
}

#[test]
fn halted_detection_is_idle_on_a_guard_stop() {
    let mut model = AgentModel::default();
    for (reason, halted) in [
        (v2::StopReason::MaxTurnRequests, true),
        (v2::StopReason::Other(STOP_REASON_DOOM_LOOP.into()), true),
        (v2::StopReason::EndTurn, false),
        (v2::StopReason::Cancelled, false),
        (v2::StopReason::Other(STOP_REASON_FAILED.into()), false),
    ] {
        apply(&mut model, running());
        assert!(!model.frame.halted_awaiting_continue());
        apply(&mut model, idle(Some(reason)));
        assert_eq!(model.frame.halted_awaiting_continue(), halted);
    }
}

#[test]
fn the_model_option_sets_the_selection() {
    let mut model = AgentModel::default();
    let value = horizon_acp::encode_model_option_id("openrouter", "anthropic/claude-x");
    let mut option_meta = None;
    write_horizon_meta(
        &mut option_meta,
        &ModelOptionMeta {
            provider: "openrouter".into(),
            model: "anthropic/claude-x".into(),
        },
    )
    .unwrap();
    let mut entry =
        v2::SessionConfigSelectOption::new(value.clone(), "openrouter · anthropic/claude-x");
    entry.meta = option_meta;
    let option = v2::SessionConfigOption::select(MODEL_CONFIG_ID, "Model", value, vec![entry]);
    apply(
        &mut model,
        v2::SessionUpdate::ConfigOptionUpdate(v2::ConfigOptionUpdate::new(vec![option])),
    );
    assert_eq!(
        model.selection,
        Some(ModelSelection {
            provider: "openrouter".into(),
            model: "anthropic/claude-x".into(),
        })
    );

    // Without the entry meta the value id is decoded.
    let value = horizon_acp::encode_model_option_id("default", "gpt-5");
    let option = v2::SessionConfigOption::select(
        MODEL_CONFIG_ID,
        "Model",
        value.clone(),
        vec![v2::SessionConfigSelectOption::new(value, "default · gpt-5")],
    );
    apply(
        &mut model,
        v2::SessionUpdate::ConfigOptionUpdate(v2::ConfigOptionUpdate::new(vec![option])),
    );
    assert_eq!(
        model.selection,
        Some(ModelSelection {
            provider: "default".into(),
            model: "gpt-5".into(),
        })
    );
}

#[test]
fn argument_progress_is_replaced_in_place_and_removed_on_close() {
    let mut model = AgentModel::default();
    for bytes in [10, 64] {
        model.apply(AgentEvent::ToolCallProgress(
            ToolCallProgressEvent::Progress {
                key: "call-1".into(),
                tool_id: Some("fs.edit".into()),
                bytes,
            },
        ));
    }
    assert!(matches!(
        model.frame.items.as_slice(),
        [AgentFrameItem::ToolCallPreparing(ToolCallPreparing {
            bytes: 64,
            ..
        })]
    ));
    model.apply(AgentEvent::ToolCallProgress(
        ToolCallProgressEvent::Closed {
            key: "call-1".into(),
        },
    ));
    assert!(model.frame.items.is_empty());
}

fn progress(id: SessionId, state: TaskProgressState, activity: Option<&str>) -> AgentEvent {
    AgentEvent::TaskProgress(TaskProgressNotification {
        session_id: SessionId::new(),
        task_session_id: id,
        description: "investigate the flaky test".to_string(),
        state,
        activity: activity.map(str::to_string),
        started_at_epoch_ms: 1_000,
    })
}

#[test]
fn running_task_progress_upserts_in_launch_order_and_finished_retires() {
    let mut model = AgentModel::default();
    let first = SessionId::new();
    let second = SessionId::new();
    model.apply(progress(first, TaskProgressState::Running, None));
    model.apply(progress(
        second,
        TaskProgressState::Running,
        Some("fs.grep"),
    ));
    model.apply(progress(first, TaskProgressState::Running, Some("fs.read")));
    assert_eq!(model.tasks.len(), 2);
    assert_eq!(model.tasks[0].task_session_id, first);
    assert_eq!(model.tasks[0].activity.as_deref(), Some("fs.read"));
    model.apply(progress(first, TaskProgressState::Finished, None));
    assert_eq!(model.tasks.len(), 1);
    assert_eq!(model.tasks[0].task_session_id, second);
    model.apply(progress(first, TaskProgressState::Finished, None));
    assert_eq!(model.tasks.len(), 1);
}
