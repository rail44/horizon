pub mod duckdb_state;
pub mod event_log;
pub mod frame;
pub mod mock;
pub mod provider;
pub mod rig;
pub mod runtime_state;
pub mod tools;
pub mod types;

pub use frame::*;
pub use mock::*;
pub use provider::*;
pub use runtime_state::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::SessionId;

    #[test]
    fn mock_agent_emits_initial_session_events() {
        let provider = MockAgentProvider::new();
        let handle = provider.start_session(StartAgentSession {
            session_id: SessionId::new(),
            provider_id: provider.provider_id(),
        });

        let first = handle.events().recv().expect("first event");
        assert_eq!(
            first.event,
            AgentEvent::StateChanged(AgentSessionState::Created)
        );
        assert_eq!(first.provider_payload, None);
    }

    #[test]
    fn transcript_renderer_keeps_provider_neutral_messages() {
        let transcript = render_agent_transcript(&[AgentEvent::MessageCommitted(AgentMessage {
            role: AgentMessageRole::Assistant,
            text: "ready".to_string(),
        })]);

        assert!(transcript.contains("assistant: ready"));
    }

    #[test]
    fn agent_frame_keeps_state_and_structured_messages() {
        let frame = agent_frame_from_events(&[
            AgentEvent::StateChanged(AgentSessionState::Running),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            }),
        ]);

        assert_eq!(frame.state, Some(AgentSessionState::Running));
        assert_eq!(
            frame.items,
            vec![AgentFrameItem::Message(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_consecutive_reasoning_deltas() {
        let frame = agent_frame_from_events(&[
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think ".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "more".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![AgentFrameItem::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think more".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_consecutive_assistant_text_deltas() {
        let frame = agent_frame_from_events(&[
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "hello ".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "world".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![AgentFrameItem::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "hello world".to_string(),
            })]
        );
    }

    #[test]
    fn agent_frame_coalesces_interleaved_stream_deltas_within_turn() {
        let frame = agent_frame_from_events(&[
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "question".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "think ".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "answer ".to_string(),
            }),
            AgentEvent::ReasoningDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "more".to_string(),
            }),
            AgentEvent::AssistantTextDelta(AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "done".to_string(),
            }),
        ]);

        assert_eq!(
            frame.items,
            vec![
                AgentFrameItem::Message(AgentMessage {
                    role: AgentMessageRole::User,
                    text: "question".to_string(),
                }),
                AgentFrameItem::ReasoningDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "think more".to_string(),
                }),
                AgentFrameItem::AssistantTextDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "answer done".to_string(),
                }),
            ]
        );
    }

    #[test]
    fn runtime_state_store_accumulates_events_into_frame() {
        let store = AgentRuntimeStateStore::new();
        let frame = store.extend_events([
            AgentEvent::StateChanged(AgentSessionState::Running),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "ready".to_string(),
            }),
        ]);

        assert_eq!(frame.state, Some(AgentSessionState::Running));
        assert_eq!(store.frame(), frame);
    }

    #[test]
    fn runtime_state_store_enqueues_events_to_jsonl_log() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-runtime-log-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let session_id = SessionId::new();
        let provider_id = AgentProviderId("builtin.agent.rig".to_string());
        let writer =
            crate::agent::event_log::AgentEventLogWriterHandle::open(&path).expect("event log");
        let store = AgentRuntimeStateStore::with_event_log(
            session_id,
            Some(provider_id.clone()),
            writer.clone(),
        );

        store.extend_provider_events([
            AgentProviderEvent::from(AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "hello".to_string(),
            })),
            AgentProviderEvent::with_provider_payload(
                AgentEvent::AssistantTextDelta(AgentMessageDelta {
                    role: AgentMessageRole::Assistant,
                    text: "hi".to_string(),
                }),
                serde_json::json!({ "delta": true }),
            ),
        ]);
        writer.flush_for_tests().expect("flush");

        let report = crate::agent::event_log::read_agent_event_log(&path).expect("read log");
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
    fn agent_frame_tracks_pending_approval_until_tool_finishes() {
        let call_id = AgentToolCallId("call-1".to_string());
        let mut frame = AgentFrame::empty();
        frame
            .items
            .push(AgentFrameItem::ApprovalRequested(AgentApprovalRequest {
                call_id: call_id.clone(),
                reason: "needs approval".to_string(),
            }));

        assert_eq!(frame.pending_approval_call_id(), Some(call_id.clone()));

        frame
            .items
            .push(AgentFrameItem::ToolCallFinished(AgentToolCallResult {
                call_id,
                output: serde_json::json!({ "ok": true }),
            }));

        assert_eq!(frame.pending_approval_call_id(), None);
    }

    #[test]
    fn horizon_policy_adds_approval_for_requested_tool() {
        let call_id = AgentToolCallId("call-1".to_string());
        let events = horizon_events_for_provider_event(&AgentEvent::ToolCallRequested(
            AgentToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "mock.approval_required".to_string(),
                input: serde_json::json!({}),
            },
        ));

        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ApprovalRequested(request) if request.call_id == call_id
        )));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::StateChanged(AgentSessionState::WaitingForApproval)
            )
        }));
    }

    #[test]
    fn mock_agent_accepts_tool_call_result_command() {
        let provider = MockAgentProvider::new();
        let handle = provider.start_session(StartAgentSession {
            session_id: SessionId::new(),
            provider_id: provider.provider_id(),
        });
        let tx = handle.sender();
        let rx = handle.events();

        let _ = tx.send(AgentCommand::ToolCallResult(AgentToolCallResult {
            call_id: AgentToolCallId("call-1".to_string()),
            output: serde_json::json!({ "ok": true }),
        }));

        let saw_ack =
            std::iter::from_fn(|| rx.recv_timeout(std::time::Duration::from_millis(50)).ok())
                .take(5)
                .any(|provider_event| {
                    matches!(
                        provider_event.event,
                        AgentEvent::MessageCommitted(AgentMessage {
                            role: AgentMessageRole::Assistant,
                            text,
                        }) if text.contains("Tool result received")
                    )
                });

        assert!(saw_ack);
    }

    #[test]
    fn provider_registry_starts_builtin_provider() {
        let registry = AgentProviderRegistry::builtin();
        let provider_id = registry.default_provider_id();
        let handle = registry
            .start_session(&provider_id, SessionId::new())
            .expect("builtin provider");

        let first = handle.events().recv().expect("first event");
        assert_eq!(
            first.event,
            AgentEvent::StateChanged(AgentSessionState::Created)
        );
    }
}
