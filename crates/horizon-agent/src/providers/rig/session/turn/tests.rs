use super::super::memory::MemoryCheckpoint;
use super::*;
use crate::config::RigAgentConfig;
use crate::contract::ProviderEvent;
use serde_json::json;

/// A completing standing-role interaction gets one reminder, even if the
/// fallback responder never calls tools. The next interaction gets its own.
#[tokio::test]
async fn memory_reminder_is_bounded_and_resets_for_the_next_interaction() {
    let (events_tx, events) = crossbeam_channel::unbounded();
    let mut state = SessionLoopState {
        memory: Some(Default::default()),
        config: RigAgentConfig {
            api_key_present: false,
            ..Default::default()
        },
        events_tx,
        ..Default::default()
    };
    for text in ["first interaction", "next interaction"] {
        state.handle_user_message(text.into()).await;
        let emitted: Vec<_> = events
            .try_iter()
            .filter_map(ProviderEvent::into_event)
            .collect();
        assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event,
                    Event::MessageCommitted(AgentMessage { role: MessageRole::AutoContinue, text })
                        if text == MEMORY_CHECKPOINT_REMINDER
                ))
                .count(),
            1
        );
        assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event, Event::MemoryCheckpointMissed))
                .count(),
            1
        );
        assert!(
            emitted
                .iter()
                .any(|event| matches!(event, Event::TurnEnded(TurnEndReason::Completed)))
        );
    }
    state.memory = None;
    state.handle_user_message("ordinary role".into()).await;
    assert!(!events.try_iter().any(|event| matches!(
        event.clone().into_event().expect("conversation event"),
        Event::MemoryCheckpointMissed
            | Event::MessageCommitted(AgentMessage {
                role: MessageRole::AutoContinue,
                ..
            })
    )));
}

#[tokio::test]
async fn accepted_memory_updates_and_no_update_declarations_satisfy_the_checkpoint() {
    for args in [
        json!({"goal": {"op": "set", "content": "ship"}}),
        json!({"no_update": {"reason": "nothing changed"}}),
    ] {
        let call_id = ToolCallId("memory".into());
        let sibling = ToolCallId("pending-sibling".into());
        let descriptor = ToolCallDescriptor {
            identity: crate::test_support::tool_identity(&call_id),
            tool_id: crate::tools::MEMORY_UPDATE_TOOL_ID.into(),
            args,
        };
        let request = crate::contract::ToolCallRequest {
            call_id: call_id.clone(),
            occurrence_id: descriptor.identity.occurrence_id.clone(),
            tool_id: descriptor.tool_id.clone(),
            input: descriptor.args.clone().into(),
        };
        let result = ToolCallResult::new(
            call_id.clone(),
            descriptor.identity.occurrence_id.clone(),
            json!({"ok": true}),
        );
        let (events_tx, events) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            memory: Some(super::super::memory::StandingMemory {
                checkpoint: MemoryCheckpoint::Reminded,
                ..Default::default()
            }),
            execution: HashMap::from([
                (call_id, descriptor),
                (
                    sibling.clone(),
                    ToolCallDescriptor {
                        identity: crate::test_support::tool_identity(&sibling),
                        tool_id: "fs_read".into(),
                        args: json!({"path": "/pending"}),
                    },
                ),
            ])
            .into(),
            guard: super::super::TurnLoopGuard::new(Some(20), 10),
            events_tx,
            ..Default::default()
        };
        state
            .rig_history
            .apply_event(&crate::providers::rig::conversation::announcement(
                &request,
                "memory-response",
            ))
            .unwrap();
        state.handle_tool_result(result).await;
        assert_eq!(
            state.memory.as_ref().unwrap().checkpoint,
            MemoryCheckpoint::Satisfied
        );
        assert!(events.try_iter().any(|event| matches!(
            event.clone().into_event().expect("conversation event"),
            Event::MemoryDigest(_)
        )));
        assert!(
            state
                .handle_memory_checkpoint(TurnCompletion::default())
                .await
                .unwrap()
                .is_completing()
        );
        assert!(
            events.try_recv().is_err(),
            "a satisfied checkpoint must not remind or report a miss"
        );
    }
}
