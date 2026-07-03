use std::{collections::HashSet, thread, time::Duration};

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};

use crate::agent::contract::*;
use crate::agent::tools::{cancelled_tool_call_result, tool_result_message};

pub(crate) struct MockProvider;

impl MockProvider {
    pub(crate) fn new() -> Self {
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

            // Tracks the tool call (if any) requested by the current turn
            // that hasn't yet received a `Command::ToolCallResult` — this is
            // what `Command::Cancel` cancels when nothing is actively
            // streaming. Only one is ever outstanding at a time in v1.
            let mut pending_tool_call: Option<ToolCallId> = None;
            // Call ids cancelled while pending, so a late `ToolCallResult`
            // for them is accepted and silently dropped rather than
            // producing a (now meaningless) acknowledgement.
            let mut cancelled_call_ids: HashSet<ToolCallId> = HashSet::new();

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
                            pending_tool_call = Some(call_id.clone());
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
                            pending_tool_call = Some(call_id.clone());
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
                        if lower_text.contains("slow") {
                            // Simulates a streaming turn that takes long
                            // enough to be interrupted, so cancellation
                            // semantics are testable without a network
                            // provider (see docs/agent-tools-design.md).
                            match run_cancellable_mock_turn(&commands_rx, &events_tx, &text) {
                                MockTurnOutcome::Completed => {
                                    let _ = events_tx.send(
                                        Event::StateChanged(SessionState::WaitingForUser).into(),
                                    );
                                }
                                MockTurnOutcome::Cancelled => {}
                                MockTurnOutcome::Shutdown => {
                                    let _ = events_tx
                                        .send(Event::StateChanged(SessionState::Terminated).into());
                                    let _ = events_tx.send(
                                        Event::Exited(Exit {
                                            reason: "shutdown".to_string(),
                                        })
                                        .into(),
                                    );
                                    break;
                                }
                            }
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
                        let Some(call_id) = pending_tool_call.take() else {
                            let _ = events_tx.send(
                                Event::MessageCommitted(Message {
                                    role: MessageRole::Assistant,
                                    text: "No active mock request to cancel.".to_string(),
                                })
                                .into(),
                            );
                            continue;
                        };
                        cancelled_call_ids.insert(call_id.clone());
                        let _ = events_tx.send(
                            Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into(),
                        );
                        let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
                        let _ = events_tx
                            .send(Event::StateChanged(SessionState::WaitingForUser).into());
                    }
                    Command::ApproveToolCall { call_id } => {
                        pending_tool_call = None;
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
                        pending_tool_call = None;
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
                        if cancelled_call_ids.remove(&result.call_id) {
                            // Accepted and silently dropped: this result
                            // belongs to a call whose turn was cancelled.
                            continue;
                        }
                        if pending_tool_call.as_ref() == Some(&result.call_id) {
                            pending_tool_call = None;
                        }
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

enum MockTurnOutcome {
    Completed,
    Cancelled,
    Shutdown,
}

const MOCK_STREAM_CHUNK_TICK: Duration = Duration::from_millis(100);

/// Simulates a streaming assistant reply, chunked word by word, so a test
/// (or a slow-fingered user) has a real window to cancel mid-turn. Whatever
/// text streamed before cancellation is committed as a partial message,
/// matching the "cancellation is a stop reason, not an error" contract.
fn run_cancellable_mock_turn(
    commands_rx: &Receiver<Command>,
    events_tx: &Sender<ProviderEvent>,
    text: &str,
) -> MockTurnOutcome {
    let full_response = format!("Mock response: {text}");
    let mut streamed = String::new();

    for word in full_response.split_inclusive(' ') {
        match commands_rx.recv_timeout(MOCK_STREAM_CHUNK_TICK) {
            Ok(Command::Cancel { .. }) => {
                if !streamed.is_empty() {
                    let _ = events_tx.send(
                        Event::MessageCommitted(Message {
                            role: MessageRole::Assistant,
                            text: streamed,
                        })
                        .into(),
                    );
                }
                let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
                let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
                return MockTurnOutcome::Cancelled;
            }
            Ok(Command::Shutdown) => {
                if !streamed.is_empty() {
                    let _ = events_tx.send(
                        Event::MessageCommitted(Message {
                            role: MessageRole::Assistant,
                            text: streamed,
                        })
                        .into(),
                    );
                }
                return MockTurnOutcome::Shutdown;
            }
            Ok(_other) => {
                // v1 is one turn at a time; other commands observed mid
                // stream are ignored rather than queued.
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        streamed.push_str(word);
        let _ = events_tx.send(
            Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: word.to_string(),
            })
            .into(),
        );
    }

    let _ = events_tx.send(
        Event::MessageCommitted(Message {
            role: MessageRole::Assistant,
            text: streamed,
        })
        .into(),
    );
    MockTurnOutcome::Completed
}
