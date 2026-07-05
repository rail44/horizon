use std::collections::{HashMap, HashSet};

use super::completion::{partial_assistant_message, TurnCompletion, MULTI_TOOL_TEST_BATCH_SIZE};
use super::mapping::{
    horizon_events_from_rig_message, horizon_provider_events_from_rig_message,
    horizon_tool_definition_from_rig, rig_messages_from_horizon_events,
    rig_tool_call_provider_payload, rig_tool_call_request, rig_workspace_snapshot_call,
    rig_workspace_snapshot_call_with_provider_metadata, RIG_PROVIDER_PAYLOAD_SCHEMA,
    RIG_PROVIDER_PAYLOAD_VERSION,
};
use super::session::{
    append_cancelled_tool_results_to_history, apply_turn_outcome, fold_batched_tool_result,
    halt_turn_loop, tool_result_fingerprint, BatchStep, GuardHalt, TurnLoopGuard,
};
use super::*;
use crate::config::RigAgentConfig;

/// Mirrors the built-in defaults in `agent::config` (`DEFAULT_ITERATION_CAP`/
/// `DEFAULT_DOOM_LOOP_WINDOW`) for these guard-logic unit tests, which
/// exercise `TurnLoopGuard` directly rather than through config precedence
/// (that precedence is covered in `agent::config`'s own tests).
const TEST_ITERATION_CAP: u32 = 25;
const TEST_DOOM_LOOP_WINDOW: usize = 3;
use crate::contract::SessionId;
use crate::contract::{
    Command, Event, Message as AgentMessage, MessageDelta, MessageRole, Provider as AgentProvider,
    ProviderEvent, ProviderId, SessionState, StartSession, ToolCallId, ToolCallRequest,
    ToolCallResult, ToolPermission, TurnEndReason,
};
use rig_core::{
    completion::{
        message::{Text, ToolCall, ToolFunction, ToolResultContent, UserContent},
        AssistantContent, Message as RigMessage, ToolDefinition,
    },
    OneOrMany,
};

fn recv(rx: &crossbeam_channel::Receiver<ProviderEvent>) -> ProviderEvent {
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .expect("expected a provider event within timeout")
}

#[test]
fn converts_rig_assistant_text_to_horizon_message() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new("hello"))),
    });

    assert!(matches!(
        events.as_slice(),
        [Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text,
        })] if text == "hello"
    ));
}

#[test]
fn emits_rig_reasoning_before_assistant_text() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::many(vec![
            AssistantContent::Text(Text::new("final answer")),
            AssistantContent::Reasoning(rig_core::completion::message::Reasoning::new(
                "thinking first",
            )),
        ])
        .expect("assistant content"),
    });

    assert!(matches!(
        events.as_slice(),
        [
            Event::ReasoningDelta(delta),
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::Assistant,
                text,
            }),
        ] if delta.text == "thinking first" && text == "final answer"
    ));
}

#[test]
fn converts_rig_tool_call_to_horizon_tool_request() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
    });

    assert!(matches!(
        events.as_slice(),
        [Event::ToolCallRequested(request)]
            if request.tool_id == "workspace.snapshot"
                && request.call_id.0 == "rig-workspace-snapshot-1"
    ));
}

#[test]
fn builds_versioned_rig_tool_call_provider_payload() {
    let call = rig_workspace_snapshot_call_with_provider_metadata();
    let payload = rig_tool_call_provider_payload(&call);

    assert_eq!(payload["schema"], RIG_PROVIDER_PAYLOAD_SCHEMA);
    assert_eq!(payload["version"], RIG_PROVIDER_PAYLOAD_VERSION);
    assert_eq!(
        payload["rig"]["tool_call"]["id"],
        "rig-workspace-snapshot-1"
    );
    assert_eq!(payload["rig"]["tool_call"]["call_id"], "provider-call-1");
    assert_eq!(payload["rig"]["tool_call"]["signature"], "signature-1");
    assert_eq!(
        payload["rig"]["tool_call"]["additional_params"]["reasoning_ref"],
        "reasoning-1"
    );
    assert_eq!(
        payload["rig"]["tool_call"]["function"]["name"],
        "workspace.snapshot"
    );
}

#[test]
fn converts_rig_tool_call_to_provider_event_with_payload() {
    let events = horizon_provider_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(
            rig_workspace_snapshot_call_with_provider_metadata(),
        )),
    });

    assert!(matches!(
        events.as_slice(),
        [ProviderEvent {
            event: Event::ToolCallRequested(request),
            provider_payload: Some(payload),
            ..
        }] if request.call_id.0 == "provider-call-1"
            && payload["schema"] == RIG_PROVIDER_PAYLOAD_SCHEMA
            && payload["rig"]["tool_call"]["id"] == "rig-workspace-snapshot-1"
    ));
}

#[test]
fn tool_call_delta_buffer_emits_progress_and_final_tool_call_still_works_unchanged() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut buffer = ToolCallProgressBuffer::new(tx, &RigAgentConfig::default());

    // A name chunk flushes immediately, before any arguments have streamed.
    buffer.note_name("internal-call-1", "fs.write".to_string());
    let progress = recv(&rx)
        .tool_call_progress
        .expect("name chunk produces a progress tick");
    assert_eq!(progress.key, "internal-call-1");
    assert_eq!(progress.tool_id.as_deref(), Some("fs.write"));
    assert_eq!(progress.bytes, 0);

    // Argument chunks accumulate bytes; `flush_for_tests` bypasses the
    // normal time-gated cadence so the test doesn't need to sleep.
    buffer.note_delta("internal-call-1", "{\"path\":\"/tmp/x\"}");
    buffer.flush_for_tests();
    let progress = recv(&rx)
        .tool_call_progress
        .expect("delta chunk produces a progress tick");
    assert_eq!(progress.tool_id.as_deref(), Some("fs.write"));
    assert_eq!(progress.bytes, "{\"path\":\"/tmp/x\"}".len());

    // The buffer is purely a side channel: a complete, non-streamed tool
    // call still maps to a normal `Event::ToolCallRequested`, not a
    // progress event.
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
    });
    assert!(matches!(
        events.as_slice(),
        [Event::ToolCallRequested(request)] if request.tool_id == "workspace.snapshot"
    ));
}

#[test]
fn duckdb_store_preserves_rig_provider_payload_for_tool_call() {
    let store = crate::persistence::projection::duckdb::Store::open_in_memory().expect("store");
    let session_id = crate::contract::SessionId::new();
    let call = rig_workspace_snapshot_call_with_provider_metadata();
    let provider_payload = rig_tool_call_provider_payload(&call);
    let event = Event::ToolCallRequested(rig_tool_call_request(call));

    store
        .append_event(crate::persistence::projection::duckdb::AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
            event,
            provider_payload: Some(provider_payload.clone()),
        })
        .expect("append rig payload event");

    let events = store.events_for_session(session_id).expect("events");
    assert_eq!(
        events[0].provider_id,
        Some(ProviderId("builtin.agent.rig".to_string()))
    );
    assert_eq!(events[0].provider_payload, Some(provider_payload));
    assert_eq!(
        store
            .tool_calls_for_session(session_id)
            .expect("tool calls")[0]
            .call_id
            .0,
        "provider-call-1"
    );
}

#[test]
fn converts_rig_tool_definition_without_leaking_rig_type() {
    let definition = horizon_tool_definition_from_rig(
        ToolDefinition {
            name: "workspace.snapshot".to_string(),
            description: "Read workspace state".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        },
        ToolPermission::AutoAllowRead,
    );

    assert_eq!(definition.id, "workspace.snapshot");
    assert_eq!(definition.permission, ToolPermission::AutoAllowRead);
}

#[test]
fn rebuilds_rig_memory_messages_from_horizon_transcript_events() {
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "snapshot please".to_string(),
        }),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "workspace.snapshot".to_string(),
            input: serde_json::json!({}),
        }),
        Event::ToolCallFinished(ToolCallResult {
            call_id: ToolCallId("call-1".to_string()),
            output: serde_json::json!({ "tab_count": 1 }),
        }),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "There is one tab.".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert!(matches!(&messages[0], RigMessage::User { .. }));
    assert!(matches!(
        &messages[1],
        RigMessage::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                if call.id == "call-1" && call.function.name == "workspace.snapshot")
    ));
    assert!(matches!(&messages[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == "call-1"
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("tab_count")))));
    assert!(matches!(&messages[3], RigMessage::Assistant { .. }));
}

#[test]
fn loads_initial_rig_history_from_duckdb_projection() {
    let path = std::env::temp_dir().join(format!(
        "horizon-rig-memory-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let session_id = crate::contract::SessionId::new();
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "hello".to_string(),
        }),
        Event::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: "streaming ignored".to_string(),
        }),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "hi".to_string(),
        }),
    ];

    {
        let store = crate::persistence::projection::duckdb::Store::open(&path).expect("open store");
        store
            .append_events(
                session_id,
                Some(ProviderId("builtin.agent.rig".to_string())),
                events.clone(),
            )
            .expect("append events");
    }

    let history = load_rig_history(Some(&path), session_id);
    assert_eq!(history, rig_messages_from_horizon_events(&events));

    let _ = std::fs::remove_file(path);
}

#[test]
fn horizon_mediated_tool_result_can_continue_as_rig_history() {
    let tool_call = rig_workspace_snapshot_call();
    let mut events = horizon_events_from_rig_message(RigMessage::from(tool_call));
    let request = match events.first().expect("tool request") {
        Event::ToolCallRequested(request) => request.clone(),
        other => panic!("expected tool request, got {other:?}"),
    };

    events.push(Event::ToolCallStarted(request.call_id.clone()));
    events.push(Event::ToolCallFinished(ToolCallResult {
        call_id: request.call_id.clone(),
        output: serde_json::json!({
            "tab_count": 1,
            "active_title": "Agent #1",
        }),
    }));

    let messages = rig_messages_from_horizon_events(&events);

    assert_eq!(messages.len(), 2);
    assert!(matches!(
        &messages[0],
        RigMessage::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                if call.id == request.call_id.0)
    ));
    assert!(matches!(&messages[1], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == request.call_id.0)));
}

#[test]
fn appends_cancelled_tool_results_after_assistant_tool_call_message() {
    let tool_call = rig_workspace_snapshot_call();
    let call_id = ToolCallId(tool_call.id.clone());
    let mut history = vec![
        RigMessage::user("snapshot please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(tool_call)),
        },
    ];

    append_cancelled_tool_results_to_history(&mut history, std::slice::from_ref(&call_id));

    // The assistant tool_calls message must be followed by one tool-result
    // message per cancelled call, or the next API request is rejected.
    assert_eq!(history.len(), 3);
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_id.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("cancelled")))));
}

#[test]
fn cancel_without_tool_calls_appends_no_history_tool_results() {
    let mut history = vec![
        RigMessage::user("hello"),
        RigMessage::assistant("partial answer"),
    ];

    append_cancelled_tool_results_to_history(&mut history, &[]);

    assert_eq!(history.len(), 2);
    assert!(matches!(&history[1], RigMessage::Assistant { content, .. }
        if matches!(content.first_ref(), AssistantContent::Text(text)
            if text.text == "partial answer")));
}

#[test]
fn cancelled_partial_assistant_message_keeps_streamed_text_and_tool_calls() {
    let message =
        partial_assistant_message(None, "partial text", vec![rig_workspace_snapshot_call()]);

    let RigMessage::Assistant { content, .. } = message else {
        panic!("expected an assistant message");
    };
    let items = content.into_iter().collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], AssistantContent::Text(text) if text.text == "partial text"));
    assert!(matches!(&items[1], AssistantContent::ToolCall(call)
        if call.id == "rig-workspace-snapshot-1"));
}

// --- Turn-loop guards -------------------------------------------------

#[test]
fn turn_loop_guard_iteration_cap_triggers_at_boundary() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }

    assert_eq!(
        guard.record_tool_turn(),
        Some(GuardHalt::IterationCapExceeded)
    );
}

#[test]
fn turn_loop_guard_iteration_cap_resets_on_reset() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    for _ in 0..TEST_ITERATION_CAP {
        guard.record_tool_turn();
    }

    guard.reset();

    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }
    assert_eq!(
        guard.record_tool_turn(),
        Some(GuardHalt::IterationCapExceeded)
    );
}

#[test]
fn turn_loop_guard_fingerprint_triggers_on_three_identical() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let fingerprint = 0xABCDu64;

    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

#[test]
fn turn_loop_guard_fingerprint_does_not_trigger_on_varying_fingerprints() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    for fingerprint in 0..(TEST_DOOM_LOOP_WINDOW as u64 * 2) {
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
}

#[test]
fn turn_loop_guard_reset_clears_fingerprint_window() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let fingerprint = 42u64;
    guard.record_fingerprint(fingerprint);
    guard.record_fingerprint(fingerprint);

    guard.reset();

    // If the window had survived the reset, this third identical
    // fingerprint would immediately trip the guard; it must not.
    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

#[test]
fn halt_turn_loop_records_real_result_and_cancels_only_other_pending_calls() {
    // Assistant turn requested two tool calls: A (whose real result just
    // arrived and tripped the guard) and B (still outstanding).
    let call_a = rig_workspace_snapshot_call();
    let call_b = ToolCall::new(
        "call-b".to_string(),
        ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/x" })),
    );
    let id_a = ToolCallId(call_a.id.clone());
    let id_b = ToolCallId(call_b.id.clone());
    let mut history = vec![
        RigMessage::user("snapshot please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::ToolCall(call_a),
                AssistantContent::ToolCall(call_b),
            ])
            .expect("assistant content"),
        },
    ];
    // The session loop removes the arrived call from pending (to look up
    // its descriptor) before halting; only B is still pending here.
    let mut pending: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::from([(
        id_b.clone(),
        ToolCallDescriptor {
            tool_id: "fs.read".to_string(),
            args: serde_json::json!({ "path": "/x" }),
        },
    )]);
    let mut cancelled: HashSet<ToolCallId> = HashSet::new();
    let arrived = ToolCallResult {
        call_id: id_a.clone(),
        output: serde_json::json!({ "tab_count": 2 }),
    };
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    for _ in 0..=TEST_ITERATION_CAP {
        guard.record_tool_turn();
    }
    let (tx, rx) = crossbeam_channel::unbounded();

    halt_turn_loop(
        GuardHalt::IterationCapExceeded,
        &mut guard,
        &tx,
        &mut history,
        &arrived,
        &mut pending,
        &mut cancelled,
    );

    // History stays API-valid: the assistant tool_calls message is followed
    // by one result per call. The arrived result keeps its REAL output (the
    // tool already executed — falsifying it as cancelled would misrepresent
    // e.g. a write already on disk); only B gets a synthetic cancelled one.
    assert_eq!(history.len(), 4);
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == id_a.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("tab_count") && !text.text.contains("cancelled")))));
    assert!(matches!(&history[3], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == id_b.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("cancelled")))));

    assert!(pending.is_empty());
    assert!(cancelled.contains(&id_b));
    assert!(
        !cancelled.contains(&id_a),
        "the real, already-executed result must not be marked cancelled"
    );

    match recv(&rx).event {
        Event::Error(error) => assert!(error.message.contains("consecutive tool-driven turns")),
        other => panic!("expected an Error event, got {other:?}"),
    }
    match recv(&rx).event {
        Event::ToolCallFinished(result) => {
            assert_eq!(
                result.call_id, id_b,
                "no contradictory cancelled ToolCallFinished for the arrived result"
            );
            assert_eq!(result.output["cancelled"], true);
        }
        other => panic!("expected ToolCallFinished, got {other:?}"),
    }
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Halted));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "halt must emit exactly Error, one cancelled finish for B, TurnEnded(Halted), and WaitingForUser"
    );

    // The guard was reset: a fresh allowance of tool turns is available.
    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }
}

/// Unit coverage for `Event::TurnEnded`'s fourth stop reason (`Failed`) --
/// the one path the other three (`Completed`/`Cancelled`/`Halted`) don't
/// exercise through a live session handle above, since triggering it for
/// real needs the rig OpenAI completion call to fail (`complete_rig_turn`'s
/// `Err` branch), not something worth wiring a real/fake network call for
/// here. `apply_turn_outcome` is where every rig turn's `TurnCompletion`
/// funnels through regardless of *why* it produced `failed: true`, so
/// driving it directly with that flag set proves the wiring
/// (`TurnEnded(Failed)` then `WaitingForUser`, nothing else) without needing
/// to reach the network-dependent code that sets the flag in production.
#[test]
fn apply_turn_outcome_emits_turn_ended_failed_for_a_failed_provider_request() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut rig_history = Vec::new();
    let mut pending_tool_calls = HashMap::new();
    let mut cancelled_call_ids = HashSet::new();

    apply_turn_outcome(
        TurnCompletion {
            failed: true,
            ..TurnCompletion::default()
        },
        &tx,
        &mut rig_history,
        &mut pending_tool_calls,
        &mut cancelled_call_ids,
    );

    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Failed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "a failed turn must emit exactly TurnEnded(Failed) then WaitingForUser"
    );
}

fn start_fallback_rig_session() -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    start_fallback_rig_session_with_config(RigAgentConfig {
        openai_enabled: false,
        model: "unused-in-fallback-mode".to_string(),
        ..Default::default()
    })
}

fn start_fallback_rig_session_with_config(
    config: RigAgentConfig,
) -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    let provider = Provider::new(config, None);
    let handle = provider.start_session(StartSession {
        session_id: SessionId::new(),
        provider_id: AgentProvider::provider_id(&provider),
    });
    let tx = handle.sender();
    let rx = handle.events();

    // Drain session-startup events (Created, init message, WaitingForUser).
    for _ in 0..3 {
        recv(&rx);
    }
    (tx, rx)
}

#[test]
fn rig_session_iteration_cap_halts_tool_loop_and_session_recovers() {
    let (tx, rx) = start_fallback_rig_session();

    // "snapshot" makes the deterministic fallback request a tool call, so
    // the session has a genuinely pending call to feed results into.
    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    let call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected a tool call request, got {other:?}"),
    };

    // Each result asks the fallback responder (via `loop_again`) to request
    // the tool again — a self-sustaining tool loop, exactly what the cap
    // exists to stop. Distinct outputs keep doom-loop detection out of the
    // way so the iteration cap is what trips.
    for i in 0..TEST_ITERATION_CAP {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult {
            call_id: call_id.clone(),
            output: serde_json::json!({ "loop_again": true, "n": i }),
        }));
        assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
        assert!(matches!(
            recv(&rx).event,
            Event::ToolCallRequested(request) if request.call_id == call_id
        ));
    }

    // The next tool-driven turn exceeds the cap: the session halts instead
    // of running it. The arrived result's REAL output goes into rig_history
    // (asserted directly in the halt_turn_loop unit test — history is not
    // observable through the session handle) and, since it already finished
    // for real app-side, no contradictory cancelled ToolCallFinished may be
    // emitted for it: the halt is exactly Error then WaitingForUser.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult {
        call_id: call_id.clone(),
        output: serde_json::json!({ "loop_again": true, "n": "final" }),
    }));
    match recv(&rx).event {
        Event::Error(error) => assert!(error.message.contains("consecutive tool-driven turns")),
        other => panic!("expected the iteration-cap error, got {other:?}"),
    }
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Halted));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser),
        "no cancelled ToolCallFinished may be emitted for the real, already-executed result"
    );

    // The session is still usable: a fresh user message runs a normal turn.
    let _ = tx.send(Command::UserMessage {
        text: "hello again".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello again"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn rig_session_drops_unsolicited_tool_result_without_running_a_turn() {
    let (tx, rx) = start_fallback_rig_session();

    // No tool call was ever requested, so this result is unsolicited: it
    // must not start a turn (which would append an orphan tool-result
    // message to rig_history) and must not advance the loop guards.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult {
        call_id: ToolCallId("never-requested".to_string()),
        output: serde_json::json!({ "ok": true }),
    }));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "an unsolicited tool result must be dropped silently, producing no events"
    );

    // The session is unaffected: a normal user turn still works.
    let _ = tx.send(Command::UserMessage {
        text: "hello".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn doom_loop_does_not_trip_on_identical_outputs_with_different_args() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let empty_matches = serde_json::json!({ "matches": [] });

    // Three distinct greps that all found nothing: identical outputs, but
    // different args — productive, non-looping calls per the design doc's
    // (tool, args, result) fingerprint.
    for pattern in ["alpha", "beta", "gamma"] {
        let fingerprint = tool_result_fingerprint(
            "fs.grep",
            &serde_json::json!({ "pattern": pattern }),
            &empty_matches,
        );
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
}

#[test]
fn doom_loop_trips_on_three_identical_tool_args_output_fingerprints() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let args = serde_json::json!({ "pattern": "alpha" });
    let output = serde_json::json!({ "matches": [] });

    let fingerprint = tool_result_fingerprint("fs.grep", &args, &output);
    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(guard.record_fingerprint(fingerprint), None);
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

// --- Parallel tool-call batching ---------------------------------------
//
// Regression coverage for the production incident (session 3aef2770) where
// a single completion requesting several parallel tool calls (MiniMax
// routinely requests 4 parallel `fs.read`s) made the session loop run one
// completion per *arriving result* instead of waiting for the whole batch:
// protocol-malformed history, a burst of stray "anything else?" turns, and
// the iteration-cap guard burning N times faster than intended.

#[test]
fn fold_batched_tool_result_holds_non_last_results_and_leaves_the_last_for_the_caller() {
    let call_a = ToolCallId("call-a".to_string());
    let call_b = ToolCallId("call-b".to_string());
    let call_c = ToolCallId("call-c".to_string());
    let mut history = vec![
        RigMessage::user("multi tool please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::ToolCall(ToolCall::new(
                    call_a.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/a" })),
                )),
                AssistantContent::ToolCall(ToolCall::new(
                    call_b.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/b" })),
                )),
                AssistantContent::ToolCall(ToolCall::new(
                    call_c.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/c" })),
                )),
            ])
            .expect("assistant content"),
        },
    ];
    let mut pending: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::from([
        (
            call_a.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/a" }),
            },
        ),
        (
            call_b.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/b" }),
            },
        ),
        (
            call_c.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/c" }),
            },
        ),
    ]);

    // First of three: two more calls are still outstanding, so the result
    // is folded directly into history (in arrival order) and no turn runs.
    pending.remove(&call_a);
    let result_a = ToolCallResult {
        call_id: call_a.clone(),
        output: serde_json::json!({ "contents": "a" }),
    };
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_a),
        BatchStep::Continue
    );
    assert_eq!(history.len(), 3);

    // Second of three: same story.
    pending.remove(&call_b);
    let result_b = ToolCallResult {
        call_id: call_b.clone(),
        output: serde_json::json!({ "contents": "b" }),
    };
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_b),
        BatchStep::Continue
    );
    assert_eq!(history.len(), 4);

    // Third and last: pending is now empty, so the caller must run a turn
    // with `result_c` as the prompt message — this function deliberately
    // leaves it out of history, so the normal turn plumbing
    // (`run_cancellable_turn`/`complete_rig_turn`) appends it right before
    // the resulting assistant message.
    pending.remove(&call_c);
    let result_c = ToolCallResult {
        call_id: call_c.clone(),
        output: serde_json::json!({ "contents": "c" }),
    };
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_c),
        BatchStep::RunTurn
    );
    assert_eq!(
        history.len(),
        4,
        "the last result is left for the caller to append via the normal turn plumbing"
    );

    // The two folded-in-advance results land in arrival order, right after
    // the assistant's tool_calls message.
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_a.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("\"a\"")))));
    assert!(matches!(&history[3], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_b.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("\"b\"")))));
}

#[test]
fn rig_session_batches_parallel_tool_results_into_one_follow_up_completion() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }
    assert_eq!(call_ids.len(), MULTI_TOOL_TEST_BATCH_SIZE);

    // Deliver all but the batch's last result: no completion may run while
    // any of the batch is still outstanding.
    for call_id in &call_ids[..call_ids.len() - 1] {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult {
            call_id: call_id.clone(),
            output: serde_json::json!({ "ok": true }),
        }));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "no completion should run while results are still outstanding"
        );
    }

    // The batch's last result completes it: exactly one follow-up
    // completion fires.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult {
        call_id: call_ids[call_ids.len() - 1].clone(),
        output: serde_json::json!({ "ok": true }),
    }));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "exactly one follow-up completion should run for the whole batch"
    );
}

#[test]
fn rig_session_cancel_mid_batch_drops_remaining_results_and_recovers() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }

    // Only the first of the batch resolves before the user cancels.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult {
        call_id: call_ids[0].clone(),
        output: serde_json::json!({ "ok": true }),
    }));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "no completion should run with results still outstanding"
    );

    let _ = tx.send(Command::Cancel { request_id: None });
    let remaining = &call_ids[1..];
    let mut cancelled_ids: HashSet<ToolCallId> = HashSet::new();
    for _ in remaining {
        match recv(&rx).event {
            Event::ToolCallFinished(result) => {
                assert_eq!(result.output["cancelled"], true);
                cancelled_ids.insert(result.call_id);
            }
            other => panic!("expected a cancelled ToolCallFinished, got {other:?}"),
        }
    }
    let remaining_ids: HashSet<ToolCallId> = remaining.iter().cloned().collect();
    assert_eq!(cancelled_ids, remaining_ids);
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Cancelled));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::Cancelled)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );

    // The real results for the cancelled calls arrive late: accepted and
    // dropped silently — no turn restart, nothing observable on the wire.
    for call_id in remaining {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult {
            call_id: call_id.clone(),
            output: serde_json::json!({ "ok": true }),
        }));
    }
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "late results for cancelled calls must drop silently"
    );

    // The session recovers: a fresh user message runs a normal turn.
    let _ = tx.send(Command::UserMessage {
        text: "hello again".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello again"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn rig_session_iteration_cap_counts_one_tool_turn_per_batch() {
    // A large `doom_loop_window` keeps doom-loop detection out of the way:
    // the deterministic multi-tool fallback repeats the same (tool, args)
    // pairs batch after batch, which would otherwise trip doom-loop
    // detection first and mask what this test is actually checking.
    let (tx, rx) = start_fallback_rig_session_with_config(RigAgentConfig {
        openai_enabled: false,
        model: "unused-in-fallback-mode".to_string(),
        iteration_cap: 2,
        doom_loop_window: 1000,
        ..Default::default()
    });

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    // Two consecutive batches (2 tool-driven completions total) must both
    // succeed under `iteration_cap: 2`. If the guard counted per *result*
    // instead of per *batch*, the very first 4-call batch would already
    // exceed the cap by its 3rd result, well before that batch even
    // finishes.
    for _ in 0..2 {
        let mut call_ids = Vec::new();
        for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
            match recv(&rx).event {
                Event::ToolCallRequested(request) => call_ids.push(request.call_id),
                other => panic!("expected a tool call request, got {other:?}"),
            }
        }
        for (index, call_id) in call_ids.iter().enumerate() {
            let is_last = index == call_ids.len() - 1;
            let output = if is_last {
                serde_json::json!({ "loop_again_batch": MULTI_TOOL_TEST_BATCH_SIZE })
            } else {
                serde_json::json!({ "index": index })
            };
            let _ = tx.send(Command::ToolCallResult(ToolCallResult {
                call_id: call_id.clone(),
                output,
            }));
            if is_last {
                assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
            } else {
                assert!(
                    rx.recv_timeout(std::time::Duration::from_millis(200))
                        .is_err(),
                    "no completion should run while results are still outstanding"
                );
            }
        }
    }

    // The 3rd tool-driven completion exceeds the cap: it must halt instead
    // of running.
    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }
    for (index, call_id) in call_ids.iter().enumerate() {
        let is_last = index == call_ids.len() - 1;
        let _ = tx.send(Command::ToolCallResult(ToolCallResult {
            call_id: call_id.clone(),
            output: serde_json::json!({ "index": index }),
        }));
        if !is_last {
            assert!(
                rx.recv_timeout(std::time::Duration::from_millis(200))
                    .is_err(),
                "no completion should run while results are still outstanding"
            );
        }
    }
    match recv(&rx).event {
        Event::Error(error) => assert!(error.message.contains("consecutive tool-driven turns")),
        other => panic!("expected the iteration-cap error, got {other:?}"),
    }
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Halted));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}
