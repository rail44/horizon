use std::thread;

use crossbeam_channel::unbounded;

use super::provider::{AgentProvider, AgentSessionHandle};
use super::types::*;
use crate::agent_tools::tool_result_message;

pub struct MockAgentProvider;

impl MockAgentProvider {
    pub fn new() -> Self {
        Self
    }
}

impl AgentProvider for MockAgentProvider {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId("builtin.agent.mock".to_string())
    }

    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded::<AgentProviderEvent>();
        let provider_id = request.provider_id.clone();

        thread::spawn(move || {
            let _ = events_tx.send(AgentEvent::StateChanged(AgentSessionState::Created).into());
            let _ = events_tx.send(
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: format!(
                        "Mock agent session started via provider `{}`.",
                        provider_id.0
                    ),
                })
                .into(),
            );
            let _ =
                events_tx.send(AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into());

            while let Ok(command) = commands_rx.recv() {
                match command {
                    AgentCommand::Initialize(_) => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::UserMessage { text } => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::User,
                                text: text.clone(),
                            })
                            .into(),
                        );
                        let lower_text = text.to_ascii_lowercase();
                        if lower_text.contains("snapshot") {
                            let call_id = AgentToolCallId("workspace-snapshot-1".to_string());
                            let _ = events_tx.send(
                                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                                    call_id,
                                    tool_id: "workspace.snapshot".to_string(),
                                    input: serde_json::json!({}),
                                })
                                .into(),
                            );
                            continue;
                        }
                        if lower_text.contains("tool") {
                            let call_id = AgentToolCallId("mock-tool-1".to_string());
                            let _ = events_tx.send(
                                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                                    call_id: call_id.clone(),
                                    tool_id: "mock.approval_required".to_string(),
                                    input: serde_json::json!({ "message": text }),
                                })
                                .into(),
                            );
                            continue;
                        }
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: format!("Mock response: {text}"),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::Cancel { .. } => {
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "No active mock request to cancel.".to_string(),
                            })
                            .into(),
                        );
                    }
                    AgentCommand::ApproveToolCall { call_id } => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::ToolRunning).into());
                        let _ = events_tx.send(AgentEvent::ToolCallStarted(call_id.clone()).into());
                        let _ = events_tx.send(
                            AgentEvent::ToolCallFinished(AgentToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": true,
                                    "result": "mock tool completed",
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "Approved mock tool completed.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::DenyToolCall { call_id, reason } => {
                        let _ = events_tx.send(
                            AgentEvent::ToolCallFinished(AgentToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": false,
                                    "reason": reason,
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::Assistant,
                                text: "Denied mock tool request.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::ToolCallResult(result) => {
                        let _ = events_tx.send(tool_result_message(&result).into());
                    }
                    AgentCommand::Shutdown => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Terminated).into());
                        let _ = events_tx.send(
                            AgentEvent::Exited(AgentExit {
                                reason: "shutdown".to_string(),
                            })
                            .into(),
                        );
                        break;
                    }
                }
            }
        });

        AgentSessionHandle::new(commands_tx, events_rx)
    }
}
