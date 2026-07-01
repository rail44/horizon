use std::thread;

use crossbeam_channel::unbounded;

use crate::agent::contract::*;
use crate::agent::tools::tool_result_message;

pub struct MockProvider;

impl MockProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Provider for MockProvider {
    fn provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.mock".to_string())
    }

    fn start_session(&self, request: StartSession) -> SessionHandle {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded::<ProviderEvent>();
        let provider_id = request.provider_id.clone();

        thread::spawn(move || {
            let _ = events_tx.send(Event::StateChanged(SessionState::Created).into());
            let _ = events_tx.send(
                Event::MessageCommitted(Message {
                    role: MessageRole::Assistant,
                    text: format!(
                        "Mock agent session started via provider `{}`.",
                        provider_id.0
                    ),
                })
                .into(),
            );
            let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());

            while let Ok(command) = commands_rx.recv() {
                match command {
                    Command::Initialize(_) => {
                        let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                        let _ = events_tx
                            .send(Event::StateChanged(SessionState::WaitingForUser).into());
                    }
                    Command::UserMessage { text } => {
                        let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                        let _ = events_tx.send(
                            Event::MessageCommitted(Message {
                                role: MessageRole::User,
                                text: text.clone(),
                            })
                            .into(),
                        );
                        let lower_text = text.to_ascii_lowercase();
                        if lower_text.contains("snapshot") {
                            let call_id = ToolCallId("workspace-snapshot-1".to_string());
                            let _ = events_tx.send(
                                Event::ToolCallRequested(ToolCallRequest {
                                    call_id,
                                    tool_id: "workspace.snapshot".to_string(),
                                    input: serde_json::json!({}),
                                })
                                .into(),
                            );
                            continue;
                        }
                        if lower_text.contains("tool") {
                            let call_id = ToolCallId("mock-tool-1".to_string());
                            let _ = events_tx.send(
                                Event::ToolCallRequested(ToolCallRequest {
                                    call_id: call_id.clone(),
                                    tool_id: "mock.approval_required".to_string(),
                                    input: serde_json::json!({ "message": text }),
                                })
                                .into(),
                            );
                            continue;
                        }
                        let _ = events_tx.send(
                            Event::MessageCommitted(Message {
                                role: MessageRole::Assistant,
                                text: format!("Mock response: {text}"),
                            })
                            .into(),
                        );
                        let _ = events_tx
                            .send(Event::StateChanged(SessionState::WaitingForUser).into());
                    }
                    Command::Cancel { .. } => {
                        let _ = events_tx.send(
                            Event::MessageCommitted(Message {
                                role: MessageRole::Assistant,
                                text: "No active mock request to cancel.".to_string(),
                            })
                            .into(),
                        );
                    }
                    Command::ApproveToolCall { call_id } => {
                        let _ =
                            events_tx.send(Event::StateChanged(SessionState::ToolRunning).into());
                        let _ = events_tx.send(Event::ToolCallStarted(call_id.clone()).into());
                        let _ = events_tx.send(
                            Event::ToolCallFinished(ToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": true,
                                    "result": "mock tool completed",
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            Event::MessageCommitted(Message {
                                role: MessageRole::Assistant,
                                text: "Approved mock tool completed.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx
                            .send(Event::StateChanged(SessionState::WaitingForUser).into());
                    }
                    Command::DenyToolCall { call_id, reason } => {
                        let _ = events_tx.send(
                            Event::ToolCallFinished(ToolCallResult {
                                call_id: call_id.clone(),
                                output: serde_json::json!({
                                    "approved": false,
                                    "reason": reason,
                                }),
                            })
                            .into(),
                        );
                        let _ = events_tx.send(
                            Event::MessageCommitted(Message {
                                role: MessageRole::Assistant,
                                text: "Denied mock tool request.".to_string(),
                            })
                            .into(),
                        );
                        let _ = events_tx
                            .send(Event::StateChanged(SessionState::WaitingForUser).into());
                    }
                    Command::ToolCallResult(result) => {
                        let _ = events_tx.send(tool_result_message(&result).into());
                    }
                    Command::Shutdown => {
                        let _ =
                            events_tx.send(Event::StateChanged(SessionState::Terminated).into());
                        let _ = events_tx.send(
                            Event::Exited(Exit {
                                reason: "shutdown".to_string(),
                            })
                            .into(),
                        );
                        break;
                    }
                }
            }
        });

        SessionHandle::new(commands_tx, events_rx)
    }
}
