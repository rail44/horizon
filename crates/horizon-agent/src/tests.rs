use crate::contract::SessionId;
use crate::prompt::{system_prompt, SessionEnvironment};
use crate::{contract as agent, frame::*, policy::horizon_events_for_provider_event};

fn recv_event(rx: &crossbeam_channel::Receiver<agent::ProviderEvent>) -> agent::ProviderEvent {
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .expect("expected a provider event within timeout")
}

#[test]
fn mock_agent_emits_initial_session_events() {
    let provider = crate::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
            role_id: None,
            workspace_root: None,
        },
    );

    let first = handle.events().recv().expect("first event");
    assert_eq!(
        first.event,
        agent::Event::StateChanged(agent::SessionState::Created)
    );
    assert_eq!(first.provider_payload, None);
}

#[test]
fn transcript_renderer_keeps_provider_neutral_messages() {
    let transcript = render_agent_transcript(&[agent::Event::MessageCommitted(agent::Message {
        role: agent::MessageRole::Assistant,
        text: "ready".to_string(),
    })]);

    assert!(transcript.contains("assistant: ready"));
}

#[test]
fn agent_frame_keeps_state_and_structured_messages() {
    let frame = agent_frame_from_events(&[
        agent::Event::StateChanged(agent::SessionState::Running),
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::Assistant,
            text: "ready".to_string(),
        }),
    ]);

    assert_eq!(frame.state, Some(agent::SessionState::Running));
    assert_eq!(
        frame.items,
        vec![AgentFrameItem::Message(agent::Message {
            role: agent::MessageRole::Assistant,
            text: "ready".to_string(),
        })]
    );
}

#[test]
fn agent_frame_coalesces_consecutive_reasoning_deltas() {
    let frame = agent_frame_from_events(&[
        agent::Event::ReasoningDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "think ".to_string(),
        }),
        agent::Event::ReasoningDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "more".to_string(),
        }),
    ]);

    assert_eq!(
        frame.items,
        vec![AgentFrameItem::ReasoningDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "think more".to_string(),
        })]
    );
}

#[test]
fn agent_frame_coalesces_consecutive_assistant_text_deltas() {
    let frame = agent_frame_from_events(&[
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "hello ".to_string(),
        }),
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "world".to_string(),
        }),
    ]);

    assert_eq!(
        frame.items,
        vec![AgentFrameItem::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "hello world".to_string(),
        })]
    );
}

#[test]
fn agent_frame_coalesces_interleaved_stream_deltas_within_turn() {
    let frame = agent_frame_from_events(&[
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "question".to_string(),
        }),
        agent::Event::ReasoningDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "think ".to_string(),
        }),
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "answer ".to_string(),
        }),
        agent::Event::ReasoningDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "more".to_string(),
        }),
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "done".to_string(),
        }),
    ]);

    assert_eq!(
        frame.items,
        vec![
            AgentFrameItem::Message(agent::Message {
                role: agent::MessageRole::User,
                text: "question".to_string(),
            }),
            AgentFrameItem::ReasoningDelta(agent::MessageDelta {
                role: agent::MessageRole::Assistant,
                text: "think more".to_string(),
            }),
            AgentFrameItem::AssistantTextDelta(agent::MessageDelta {
                role: agent::MessageRole::Assistant,
                text: "answer done".to_string(),
            }),
        ]
    );
}

#[test]
fn runtime_state_store_accumulates_events_into_frame() {
    let store = crate::live::LiveState::new();
    let frame = store.extend_events([
        agent::Event::StateChanged(agent::SessionState::Running),
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::Assistant,
            text: "ready".to_string(),
        }),
    ]);

    assert_eq!(frame.state, Some(agent::SessionState::Running));
    assert_eq!(store.frame(), frame);
}

/// `LiveState::with_event_log_and_history` is `horizon-sessiond`'s seam for
/// resuming a persisted session at startup (`docs/agent-runtime-split-
/// design.md` step 4): the seeded history must show up in the very first
/// frame (not just after a fresh event arrives) and in `events()` — the
/// exact list `session_load` re-emits to a (re)connecting client.
#[test]
fn with_event_log_and_history_seeds_the_frame_and_events_up_front() {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-seeded-log-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let session_id = SessionId::new();
    let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
    let history = vec![
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "hello".to_string(),
        }),
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::Assistant,
            text: "hi there".to_string(),
        }),
    ];
    let store = crate::live::LiveState::with_event_log_and_history(
        session_id,
        None,
        None,
        writer.clone(),
        history.clone(),
    );

    // Seeded up front, before any new event has been folded in.
    assert_eq!(store.events(), history);
    assert_eq!(
        store.frame(),
        crate::frame::agent_frame_from_events(&history)
    );

    // New events append on top of the seeded history, in both places.
    let frame = store.extend_provider_events([agent::ProviderEvent::from(
        agent::Event::StateChanged(agent::SessionState::WaitingForUser),
    )]);
    assert_eq!(frame.state, Some(agent::SessionState::WaitingForUser));
    assert_eq!(store.events().len(), 3);
    assert_eq!(store.events()[..2], history[..]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn runtime_state_store_enqueues_events_to_jsonl_log() {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-runtime-log-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let session_id = SessionId::new();
    let provider_id = agent::ProviderId("builtin.agent.rig".to_string());
    let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
    let store = crate::live::LiveState::with_event_log(
        session_id,
        Some(provider_id.clone()),
        None,
        writer.clone(),
    );

    store.extend_provider_events([
        agent::ProviderEvent::from(agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "hello".to_string(),
        })),
        agent::ProviderEvent::with_provider_payload(
            agent::Event::AssistantTextDelta(agent::MessageDelta {
                role: agent::MessageRole::Assistant,
                text: "hi".to_string(),
            }),
            serde_json::json!({ "delta": true }),
        ),
    ]);
    writer.flush().expect("flush");

    let report = crate::persistence::event_log::read(&path).expect("read log");
    assert_eq!(report.records.len(), 2);
    assert_eq!(report.records[0].session_id, session_id);
    assert_eq!(report.records[0].provider_id, Some(provider_id));
    assert_eq!(report.records[0].event_kind, "message_committed");
    assert_eq!(report.records[1].event_kind, "assistant_text_delta");
    assert_eq!(
        report.records[1].provider_payload,
        Some(serde_json::json!({ "delta": true }))
    );
    assert_eq!(report.records[0].turn_id, report.records[1].turn_id);
    assert!(report.records[0].turn_id.is_some());

    let _ = std::fs::remove_file(path);
}

#[test]
fn runtime_state_store_folds_tool_call_progress_but_excludes_it_from_the_jsonl_log() {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-runtime-progress-log-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let session_id = SessionId::new();
    let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
    let store = crate::live::LiveState::with_event_log(session_id, None, None, writer.clone());

    let frame = store.extend_provider_events([
        agent::ProviderEvent::tool_call_progress(agent::ToolCallProgress {
            key: "call-1".to_string(),
            tool_id: Some("fs.write".to_string()),
            bytes: 128,
        }),
        agent::ProviderEvent::from(agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "hello".to_string(),
        })),
    ]);
    writer.flush().expect("flush");

    // It folds into the frame as an ephemeral `ToolCallPreparing` item...
    assert!(frame.items.iter().any(|item| matches!(
        item,
        AgentFrameItem::ToolCallPreparing(progress)
            if progress.key == "call-1" && progress.bytes == 128
    )));

    // ...but only the real event reaches the persisted log.
    let report = crate::persistence::event_log::read(&path).expect("read log");
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].event_kind, "message_committed");

    let _ = std::fs::remove_file(path);
}

#[test]
fn tool_call_progress_updates_in_place_then_is_superseded_by_the_real_request() {
    let mut frame = AgentFrame::empty();

    apply_tool_call_progress_to_frame(
        &mut frame,
        agent::ToolCallProgress {
            key: "call-1".to_string(),
            tool_id: None,
            bytes: 16,
        },
    );
    assert_eq!(frame.items.len(), 1);
    assert!(matches!(
        &frame.items[0],
        AgentFrameItem::ToolCallPreparing(progress) if progress.bytes == 16
    ));

    // A second tick for the same call updates the existing item in place
    // rather than appending a new one.
    apply_tool_call_progress_to_frame(
        &mut frame,
        agent::ToolCallProgress {
            key: "call-1".to_string(),
            tool_id: Some("fs.write".to_string()),
            bytes: 96,
        },
    );
    assert_eq!(frame.items.len(), 1);
    assert!(matches!(
        &frame.items[0],
        AgentFrameItem::ToolCallPreparing(progress)
            if progress.bytes == 96 && progress.tool_id.as_deref() == Some("fs.write")
    ));

    // Once the real tool call arrives, it replaces the preparing item
    // rather than leaving it dangling in the transcript.
    apply_agent_event_to_frame(
        &mut frame,
        &agent::Event::ToolCallRequested(agent::ToolCallRequest {
            call_id: agent::ToolCallId("call-1".to_string()),
            tool_id: "fs.write".to_string(),
            input: serde_json::json!({ "path": "/tmp/x" }),
        }),
        &mut TurnClock::new(),
    );
    assert_eq!(frame.items.len(), 1);
    assert!(matches!(
        &frame.items[0],
        AgentFrameItem::ToolCallRequested(request) if request.tool_id == "fs.write"
    ));
}

/// `docs/agent-output-ui-amendment.md`'s 2026-07-12 addendum (turn
/// receipts): a turn's `TurnEnded` fold must carry the end reason, the
/// model id reported by the turn's `ProviderRequestSent`, and an elapsed
/// duration measured from the turn's opening `MessageCommitted(User)`.
#[test]
fn turn_ended_folds_a_receipt_with_reason_model_and_elapsed() {
    let mut frame = AgentFrame::empty();
    let mut turn = TurnClock::new();

    apply_agent_event_to_frame(
        &mut frame,
        &agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "question".to_string(),
        }),
        &mut turn,
    );
    apply_agent_event_to_frame(
        &mut frame,
        &agent::Event::ProviderRequestSent(agent::ProviderRequestSent {
            model: "gpt-4o-mini".to_string(),
        }),
        &mut turn,
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    apply_agent_event_to_frame(
        &mut frame,
        &agent::Event::TurnEnded(agent::TurnEndReason::Completed),
        &mut turn,
    );

    match frame.items.last().expect("a TurnEnded item") {
        AgentFrameItem::TurnEnded {
            reason,
            model,
            elapsed,
        } => {
            assert_eq!(*reason, agent::TurnEndReason::Completed);
            assert_eq!(model.as_deref(), Some("gpt-4o-mini"));
            assert!(
                *elapsed >= std::time::Duration::from_millis(5),
                "elapsed {elapsed:?} should cover the sleep between request and turn end"
            );
        }
        other => panic!("expected a TurnEnded item, got {other:?}"),
    }
}

/// A turn that ends before any `ProviderRequestSent` (e.g. an immediate
/// cancel) has no model to report -- `model` must read back `None`, not a
/// stale value from an earlier turn.
#[test]
fn turn_ended_with_no_provider_request_has_no_model() {
    let frame = agent_frame_from_events(&[
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text: "question".to_string(),
        }),
        agent::Event::TurnEnded(agent::TurnEndReason::Cancelled),
    ]);

    match frame.items.last().expect("a TurnEnded item") {
        AgentFrameItem::TurnEnded { reason, model, .. } => {
            assert_eq!(*reason, agent::TurnEndReason::Cancelled);
            assert_eq!(*model, None);
        }
        other => panic!("expected a TurnEnded item, got {other:?}"),
    }
}

/// Old-log replay compat for the elapsed-time trade-off (`TurnClock`'s doc
/// comment): a `TurnEnded` with nothing preceding it in the replayed slice
/// (a legacy pre-turn_id log, or a resumed-history slice starting mid-turn)
/// must still fold without panicking, reading back a near-zero elapsed and
/// no model rather than reusing stale state.
#[test]
fn agent_frame_from_events_folds_a_turn_ended_with_no_preceding_events() {
    let frame =
        agent_frame_from_events(&[agent::Event::TurnEnded(agent::TurnEndReason::Cancelled)]);

    match frame.items.as_slice() {
        [AgentFrameItem::TurnEnded {
            reason,
            model,
            elapsed,
        }] => {
            assert_eq!(*reason, agent::TurnEndReason::Cancelled);
            assert_eq!(*model, None);
            assert_eq!(*elapsed, std::time::Duration::ZERO);
        }
        other => panic!("expected exactly one TurnEnded item, got {other:?}"),
    }
}

/// `AgentFrameItem::TurnEnded` is itself a turn boundary (`is_turn_boundary_item`):
/// content folded after it must not coalesce backward across it into
/// whatever was accumulating before the turn ended.
#[test]
fn turn_ended_is_a_turn_boundary_for_coalescing() {
    let frame = agent_frame_from_events(&[
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "before".to_string(),
        }),
        agent::Event::TurnEnded(agent::TurnEndReason::Completed),
        agent::Event::AssistantTextDelta(agent::MessageDelta {
            role: agent::MessageRole::Assistant,
            text: "after".to_string(),
        }),
    ]);

    let deltas: Vec<&str> = frame
        .items
        .iter()
        .filter_map(|item| match item {
            AgentFrameItem::AssistantTextDelta(delta) => Some(delta.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec!["before", "after"],
        "the post-TurnEnded delta must not merge into the pre-TurnEnded one"
    );
}

/// `docs/agent-output-ui-amendment.md`'s decision 5 (failure display) needs
/// an explicit success/failure outcome on a finished tool call rather than
/// the UI sniffing `output`'s `"is_error"` convention itself --
/// `ToolCallResult::new` is the one place that convention is read.
#[test]
fn tool_call_result_new_derives_is_error_from_the_output_convention() {
    let ok = agent::ToolCallResult::new(
        agent::ToolCallId("call-1".to_string()),
        serde_json::json!({ "ok": true }),
    );
    assert!(!ok.is_error);

    let failed = agent::ToolCallResult::new(
        agent::ToolCallId("call-2".to_string()),
        serde_json::json!({ "is_error": true, "message": "boom" }),
    );
    assert!(failed.is_error);

    // Folded straight into the frame item, so the UI reads it off
    // `AgentFrameItem::ToolCallFinished`'s `ToolCallResult` directly.
    let frame = agent_frame_from_events(&[agent::Event::ToolCallFinished(failed.clone())]);
    match frame.items.as_slice() {
        [AgentFrameItem::ToolCallFinished(result)] => assert!(result.is_error),
        other => panic!("expected exactly one ToolCallFinished item, got {other:?}"),
    }
}

/// `ToolCallResult::denied` (the explicit denial marker replacing the old
/// message-text convention -- see `src/agent/turns.rs`'s `is_denied`) is
/// additive with `#[serde(default)]`, exactly like `is_error` was when it
/// was added: a JSON record persisted before the `denied` field existed
/// (no `"denied"` key at all) must still deserialize, reading `false`
/// regardless of the record's real outcome -- that's precisely the gap
/// `is_denied`'s message-text fallback exists to cover on replay.
#[test]
fn tool_call_result_denied_field_defaults_to_false_for_a_pre_marker_record() {
    let pre_marker_json = serde_json::json!({
        "call_id": "call-1",
        "output": { "is_error": true, "message": "denied by user" },
        "is_error": true
        // no "denied" key: this is what a record persisted before the
        // marker field existed looks like on disk.
    });

    let result: agent::ToolCallResult = serde_json::from_value(pre_marker_json).unwrap();
    assert!(!result.denied);
    assert!(result.is_error);

    // A fresh record built via the marker-setting constructor round-trips
    // with `denied: true` intact.
    let denied = agent::ToolCallResult::denied(
        agent::ToolCallId("call-2".to_string()),
        serde_json::json!({ "is_error": true, "message": "denied by user" }),
    );
    let round_tripped: agent::ToolCallResult =
        serde_json::from_str(&serde_json::to_string(&denied).unwrap()).unwrap();
    assert!(round_tripped.denied);
}

/// Compatibility: a `ToolCallResult` persisted before `is_error` existed as
/// a field has no such key in its JSON at all -- `#[serde(default)]` must
/// still deserialize it (as `false`, i.e. success), not treat the record as
/// corrupt. Mirrors `persistence::event_log`'s own
/// `reads_a_pre_role_record_with_no_role_id_key` regression guard for the
/// same additive-field shape.
#[test]
fn tool_call_result_deserializes_a_pre_is_error_record_as_success() {
    let pre_field_json = serde_json::json!({
        "call_id": "call-1",
        "output": { "is_error": true, "message": "written before is_error existed" }
    });

    let result: agent::ToolCallResult =
        serde_json::from_value(pre_field_json).expect("deserialize pre-is_error record");

    assert_eq!(result.call_id, agent::ToolCallId("call-1".to_string()));
    assert!(
        !result.is_error,
        "a pre-existing record with no is_error key must default to false, \
         even though its own output JSON says otherwise -- old records simply \
         don't get the new field's benefit until re-derived"
    );
}

#[test]
fn state_entry_advance_keeps_timestamp_until_state_changes() {
    let entry = StateEntry::initial(Some(agent::SessionState::Running));
    let entered_at = entry.entered_at();

    std::thread::sleep(std::time::Duration::from_millis(5));
    let same_state = entry.advance(Some(agent::SessionState::Running));
    assert_eq!(same_state.entered_at(), entered_at);

    let changed_state = entry.advance(Some(agent::SessionState::WaitingForUser));
    assert!(changed_state.entered_at() > entered_at);
    assert_eq!(
        changed_state.state,
        Some(agent::SessionState::WaitingForUser)
    );
}

#[test]
fn state_entry_elapsed_grows_with_time() {
    // Mirrors the shape of `src/agent/view.rs`'s running-card
    // elapsed-seconds ticker (`RunningTurnClock`): `Instant::now() -
    // started_at`, driven by a periodic tick rather than a method on the
    // clock type itself. `StateEntry` is test-only today (see its own
    // doc comment) -- this pins the timing arithmetic in isolation.
    let entry = StateEntry::initial(Some(agent::SessionState::ToolRunning));
    std::thread::sleep(std::time::Duration::from_millis(15));

    let elapsed = std::time::Instant::now().saturating_duration_since(entry.entered_at());
    assert!(elapsed >= std::time::Duration::from_millis(10));
}

#[test]
fn agent_frame_tracks_pending_approval_until_tool_finishes() {
    let call_id = agent::ToolCallId("call-1".to_string());
    let mut frame = AgentFrame::empty();
    frame
        .items
        .push(AgentFrameItem::ApprovalRequested(agent::ApprovalRequest {
            call_id: call_id.clone(),
            reason: "needs approval".to_string(),
            kind: agent::ApprovalKind::Standard,
        }));

    assert_eq!(frame.pending_approval_call_id(), Some(call_id.clone()));

    frame.items.push(AgentFrameItem::ToolCallFinished(
        agent::ToolCallResult::new(call_id, serde_json::json!({ "ok": true })),
    ));

    assert_eq!(frame.pending_approval_call_id(), None);
}

#[test]
fn agent_frame_lists_multiple_pending_approvals_oldest_first() {
    // Two calls request approval before either resolves -- dispatch acts
    // on the oldest pending call first (`AgentSession::approve`/`deny`),
    // so this test pins both the queue's order and its length.
    let first = agent::ToolCallId("call-1".to_string());
    let second = agent::ToolCallId("call-2".to_string());
    let mut frame = AgentFrame::empty();
    frame
        .items
        .push(AgentFrameItem::ApprovalRequested(agent::ApprovalRequest {
            call_id: first.clone(),
            reason: "first".to_string(),
            kind: agent::ApprovalKind::Standard,
        }));
    frame
        .items
        .push(AgentFrameItem::ApprovalRequested(agent::ApprovalRequest {
            call_id: second.clone(),
            reason: "second".to_string(),
            kind: agent::ApprovalKind::Standard,
        }));

    assert_eq!(
        frame.pending_approval_call_ids(),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(frame.pending_approval_call_id(), Some(first.clone()));

    frame.items.push(AgentFrameItem::ToolCallFinished(
        agent::ToolCallResult::new(first, serde_json::json!({ "ok": true })),
    ));

    assert_eq!(frame.pending_approval_call_ids(), vec![second.clone()]);
    assert_eq!(frame.pending_approval_call_id(), Some(second));
}

#[test]
fn horizon_policy_adds_approval_for_requested_tool() {
    let call_id = agent::ToolCallId("call-1".to_string());
    let tool_state = crate::tools::ToolSessionState::new(std::env::temp_dir());
    let events = horizon_events_for_provider_event(
        &agent::Event::ToolCallRequested(agent::ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "mock.approval_required".to_string(),
            input: serde_json::json!({}),
        }),
        &tool_state,
    );

    assert!(events.iter().any(|event| matches!(
        event,
        agent::Event::ApprovalRequested(request) if request.call_id == call_id
    )));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            agent::Event::StateChanged(agent::SessionState::WaitingForApproval)
        )
    }));
}

#[test]
fn mock_agent_accepts_tool_call_result_command() {
    let provider = crate::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
            role_id: None,
            workspace_root: None,
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    let _ = tx.send(agent::Command::ToolCallResult(agent::ToolCallResult::new(
        agent::ToolCallId("call-1".to_string()),
        serde_json::json!({ "ok": true }),
    )));

    let saw_ack = std::iter::from_fn(|| rx.recv_timeout(std::time::Duration::from_millis(50)).ok())
        .take(5)
        .any(|provider_event| {
            matches!(
                provider_event.event,
                agent::Event::MessageCommitted(agent::Message {
                    role: agent::MessageRole::Assistant,
                    text,
                }) if text.contains("Tool result received")
            )
        });

    assert!(saw_ack);
}

#[test]
fn mock_agent_cancel_mid_turn_keeps_partial_and_marks_cancelled() {
    let provider = crate::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
            role_id: None,
            workspace_root: None,
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    // Drain session-startup events (Created, init message, WaitingForUser).
    for _ in 0..3 {
        recv_event(&rx);
    }

    let _ = tx.send(agent::Command::UserMessage {
        text: "slow please".to_string(),
    });

    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::Running)
    );
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            ..
        })
    ));

    // The provider-request lifecycle markers (see `Event::ProviderRequestSent`'s
    // doc comment) bracket the turn before any streamed content: sent, then
    // first token, matching the order asserted end-to-end in
    // `mock_agent_slow_turn_emits_provider_request_lifecycle_in_order`.
    match recv_event(&rx).event {
        agent::Event::ProviderRequestSent(sent) => assert_eq!(sent.model, "mock"),
        other => panic!("expected ProviderRequestSent, got {other:?}"),
    }
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::ProviderRequestFirstToken
    );

    // Cancel as soon as the first streamed chunk arrives, well before the
    // mock's simulated turn would finish on its own.
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::AssistantTextDelta(_)
    ));
    let _ = tx.send(agent::Command::Cancel { request_id: None });

    let mut partial_commit = None;
    let mut saw_cancelled_state = false;
    let mut saw_request_finished = false;
    loop {
        match recv_event(&rx).event {
            agent::Event::AssistantTextDelta(_) => {}
            agent::Event::ProviderRequestFinished => saw_request_finished = true,
            agent::Event::MessageCommitted(agent::Message {
                role: agent::MessageRole::Assistant,
                text,
            }) => partial_commit = Some(text),
            agent::Event::StateChanged(agent::SessionState::Cancelled) => {
                saw_cancelled_state = true;
            }
            agent::Event::StateChanged(agent::SessionState::WaitingForUser) => break,
            agent::Event::Error(error) => {
                panic!(
                    "cancellation must not surface as an error: {}",
                    error.message
                )
            }
            other => panic!("unexpected event during cancellation: {other:?}"),
        }
    }

    assert!(saw_cancelled_state, "expected a Cancelled state transition");
    assert!(
        saw_request_finished,
        "expected a ProviderRequestFinished marker even when cancelled mid-turn"
    );
    let partial = partial_commit.expect("partial assistant text committed on cancel");
    assert!(!partial.is_empty());
    let full_response = "Mock response: slow please";
    assert!(
        full_response.starts_with(&partial) && partial != full_response,
        "expected a strict partial prefix of {full_response:?}, got {partial:?}"
    );
}

#[test]
fn mock_agent_slow_turn_emits_provider_request_lifecycle_in_order() {
    let provider = crate::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
            role_id: None,
            workspace_root: None,
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    // Drain session-startup events (Created, init message, WaitingForUser).
    for _ in 0..3 {
        recv_event(&rx);
    }

    let _ = tx.send(agent::Command::UserMessage {
        text: "slow please".to_string(),
    });

    #[derive(Debug, PartialEq)]
    enum Marker {
        Sent,
        FirstToken,
        Finished,
    }

    let mut markers = Vec::new();
    loop {
        match recv_event(&rx).event {
            agent::Event::ProviderRequestSent(sent) => {
                assert_eq!(sent.model, "mock");
                markers.push(Marker::Sent);
            }
            agent::Event::ProviderRequestFirstToken => markers.push(Marker::FirstToken),
            agent::Event::ProviderRequestFinished => markers.push(Marker::Finished),
            agent::Event::StateChanged(agent::SessionState::WaitingForUser) => break,
            _ => {}
        }
    }

    assert_eq!(
        markers,
        vec![Marker::Sent, Marker::FirstToken, Marker::Finished],
        "a turn must report the provider request lifecycle in sent -> first-token -> \
         finished order, exactly once each"
    );
}

#[test]
fn mock_agent_cancel_marks_pending_approval_cancelled_and_recovers() {
    let provider = crate::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
            role_id: None,
            workspace_root: None,
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    for _ in 0..3 {
        recv_event(&rx);
    }

    let _ = tx.send(agent::Command::UserMessage {
        text: "please use a tool".to_string(),
    });
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::Running)
    );
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            ..
        })
    ));
    let call_id = match recv_event(&rx).event {
        agent::Event::ToolCallRequested(request) => {
            assert_eq!(request.tool_id, "mock.approval_required");
            request.call_id
        }
        other => panic!("expected a tool call request, got {other:?}"),
    };

    // Cancel while the approval is still pending.
    let _ = tx.send(agent::Command::Cancel { request_id: None });

    match recv_event(&rx).event {
        agent::Event::ToolCallFinished(result) => {
            assert_eq!(result.call_id, call_id);
            assert_eq!(result.output["cancelled"], true);
        }
        other => panic!("expected the pending tool call to finish as cancelled, got {other:?}"),
    }
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::Cancelled)
    );
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::WaitingForUser)
    );

    // A tool result arriving late for the cancelled call is accepted and
    // silently dropped — no further events are produced for it.
    let _ = tx.send(agent::Command::ToolCallResult(agent::ToolCallResult::new(
        call_id,
        serde_json::json!({ "ignored": true }),
    )));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "a late tool call result after cancel must be silently dropped"
    );

    // The session still accepts a new user message after the cancelled turn.
    let _ = tx.send(agent::Command::UserMessage {
        text: "hello again".to_string(),
    });
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::Running)
    );
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::User,
            text,
        }) if text == "hello again"
    ));
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::MessageCommitted(agent::Message {
            role: agent::MessageRole::Assistant,
            text,
        }) if text == "Mock response: hello again"
    ));
    assert_eq!(
        recv_event(&rx).event,
        agent::Event::StateChanged(agent::SessionState::WaitingForUser)
    );
}

#[test]
fn system_prompt_reports_environment_facts() {
    let environment = SessionEnvironment {
        cwd: std::path::PathBuf::from("/home/user/project"),
        os: "linux",
        git_repo: true,
    };

    let prompt = system_prompt(&environment, &[]);

    assert!(prompt.contains("/home/user/project"));
    assert!(prompt.contains("linux"));
    assert!(prompt.contains("Git repository: yes"));
}

#[test]
fn system_prompt_reports_non_git_directory() {
    let environment = SessionEnvironment {
        cwd: std::path::PathBuf::from("/tmp"),
        os: "macos",
        git_repo: false,
    };

    let prompt = system_prompt(&environment, &[]);

    assert!(prompt.contains("Git repository: no"));
}

#[test]
fn system_prompt_stays_within_line_budget() {
    // docs/agent-tools-design.md's "System Prompt" section calls for a lean
    // prompt (~30 lines) — no step-by-step workflow prescriptions.
    const LINE_BUDGET: usize = 30;
    let environment = SessionEnvironment {
        cwd: std::path::PathBuf::from("/home/user/project"),
        os: "linux",
        git_repo: true,
    };

    let line_count = system_prompt(&environment, &[]).lines().count();

    assert!(
        line_count <= LINE_BUDGET,
        "system prompt grew to {line_count} lines, budget is {LINE_BUDGET}"
    );
}

#[test]
fn system_prompt_carries_communication_and_verification_norms() {
    // The 2026-07-07 owner decision: model-agnostic behavior norms only
    // (conciseness, faithful reporting, verify-before-done, session
    // persistence) -- see prompt.rs's module doc. This pins their presence
    // without pinning wording.
    let prompt = system_prompt(
        &SessionEnvironment {
            cwd: std::path::PathBuf::from("/repo"),
            os: "linux",
            git_repo: true,
        },
        &[],
    );

    let lower = prompt.to_ascii_lowercase();
    assert!(lower.contains("be concise"));
    assert!(lower.contains("report outcomes faithfully"));
    assert!(lower.contains("before reporting work as done, verify"));
    assert!(lower.contains("survives application restarts"));
}

#[test]
fn system_prompt_carries_tool_policy_and_retry_nudge() {
    let prompt = system_prompt(
        &SessionEnvironment {
            cwd: std::path::PathBuf::from("/repo"),
            os: "linux",
            git_repo: true,
        },
        &[],
    );

    let lower = prompt.to_ascii_lowercase();
    assert!(lower.contains("absolute path"));
    assert!(lower.contains("retry"));
}

#[test]
fn system_prompt_carries_destructive_action_caution() {
    let prompt = system_prompt(
        &SessionEnvironment {
            cwd: std::path::PathBuf::from("/repo"),
            os: "linux",
            git_repo: true,
        },
        &[],
    );

    assert!(prompt.to_ascii_lowercase().contains("destructive"));
}

#[test]
fn system_prompt_with_no_extra_sections_matches_the_base_prompt_exactly() {
    let environment = SessionEnvironment {
        cwd: std::path::PathBuf::from("/repo"),
        os: "linux",
        git_repo: true,
    };

    // An empty slice must reproduce today's prompt byte-for-byte -- the
    // back-compatibility guarantee of the `extra_sections` injection point
    // (`docs/research/agent-prompting.md` Part 2.5).
    assert_eq!(
        system_prompt(&environment, &[]),
        system_prompt(&environment, &Vec::new())
    );
    assert!(!system_prompt(&environment, &[]).contains("Repository instructions"));
}

#[test]
fn system_prompt_appends_extra_sections_after_the_base_prompt() {
    let environment = SessionEnvironment {
        cwd: std::path::PathBuf::from("/repo"),
        os: "linux",
        git_repo: true,
    };
    let base = system_prompt(&environment, &[]);
    let extra_sections = vec!["Repository instructions (AGENTS.md):\n\nRun the tests.".to_string()];

    let prompt = system_prompt(&environment, &extra_sections);

    assert!(prompt.starts_with(&base));
    assert!(prompt.ends_with("Run the tests."));
    assert!(prompt.len() > base.len());
}

#[test]
fn environment_for_workspace_root_uses_the_given_root_not_process_cwd() {
    // The 2026-07-19 dogfooding bug: an isolated session's prompt reported
    // the daemon process's own cwd as its working directory. A session's
    // real root (e.g. an isolated worktree) must win over the process cwd.
    let root = std::path::PathBuf::from("/tmp/some-isolated-worktree");

    let environment = SessionEnvironment::for_workspace_root(Some(&root));

    assert_eq!(environment.cwd, root);
    assert_ne!(environment.cwd, std::env::current_dir().unwrap());
}

#[test]
fn environment_for_workspace_root_falls_back_to_process_cwd_when_none() {
    let environment = SessionEnvironment::for_workspace_root(None);

    assert_eq!(environment.cwd, std::env::current_dir().unwrap());
}

#[test]
fn system_prompt_built_from_workspace_root_reports_the_session_root() {
    let root = std::path::PathBuf::from("/tmp/some-isolated-worktree");
    let environment = SessionEnvironment::for_workspace_root(Some(&root));

    let prompt = system_prompt(&environment, &[]);

    assert!(prompt.contains("/tmp/some-isolated-worktree"));
}

#[test]
fn provider_registry_starts_builtin_provider() {
    let registry = agent::ProviderRegistry::builtin();
    let provider_id = registry.default_provider_id();
    let handle = registry
        .start_session(&provider_id, SessionId::new(), None, None)
        .expect("builtin provider");

    let first = handle.events().recv().expect("first event");
    assert_eq!(
        first.event,
        agent::Event::StateChanged(agent::SessionState::Created)
    );
}
