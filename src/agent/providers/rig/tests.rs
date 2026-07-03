use super::mapping::{
    horizon_events_from_rig_message, horizon_provider_events_from_rig_message,
    horizon_tool_definition_from_rig, rig_messages_from_horizon_events,
    rig_tool_call_provider_payload, rig_tool_call_request, rig_workspace_snapshot_call,
    rig_workspace_snapshot_call_with_provider_metadata, RIG_PROVIDER_PAYLOAD_SCHEMA,
    RIG_PROVIDER_PAYLOAD_VERSION,
};
use super::*;
use crate::agent::contract::{
    Event, Message as AgentMessage, MessageDelta, MessageRole, ProviderEvent, ProviderId,
    ToolCallId, ToolCallRequest, ToolCallResult, ToolPermission,
};
use rig_core::{
    completion::{
        message::{Text, ToolResultContent, UserContent},
        AssistantContent, Message as RigMessage, ToolDefinition,
    },
    OneOrMany,
};

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
        }] if request.call_id.0 == "provider-call-1"
            && payload["schema"] == RIG_PROVIDER_PAYLOAD_SCHEMA
            && payload["rig"]["tool_call"]["id"] == "rig-workspace-snapshot-1"
    ));
}

#[test]
fn duckdb_store_preserves_rig_provider_payload_for_tool_call() {
    let store =
        crate::agent::persistence::projection::duckdb::Store::open_in_memory().expect("store");
    let session_id = crate::session::SessionId::new();
    let call = rig_workspace_snapshot_call_with_provider_metadata();
    let provider_payload = rig_tool_call_provider_payload(&call);
    let event = Event::ToolCallRequested(rig_tool_call_request(call));

    store
        .append_event(crate::agent::persistence::projection::duckdb::AppendEvent {
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
    let session_id = crate::session::SessionId::new();
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
        let store =
            crate::agent::persistence::projection::duckdb::Store::open(&path).expect("open store");
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
