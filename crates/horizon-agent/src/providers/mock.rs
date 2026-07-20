use std::{collections::HashSet, thread, time::Duration};

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};

use crate::contract::*;
use crate::roles::RoleId;
use crate::tools::{cancelled_tool_call_result, tool_result_message};

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

    /// The mock provider has no real model concept -- most of its turns
    /// never emit `Event::ProviderRequestSent` at all (only the "slow" test
    /// path does, with a hardcoded `"mock"` string not representative of a
    /// session's actual model), so there is nothing honest to report here.
    fn resolved_model(&self, _role_id: Option<&RoleId>) -> Option<String> {
        None
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
                    // Skew catch-all: dropped, mirroring the rig
                    // provider's session loop.
                    Command::Unknown(_) => {}
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
                        if lower_text.contains("streaming tool") {
                            // Manual test hook for tool-call-argument
                            // streaming feedback (see `ToolCallProgressBuffer`
                            // in the rig provider): simulates a slow-to-
                            // stream tool call by emitting a few
                            // `ToolCallProgress` ticks — first the tool name,
                            // then growing byte counts — before the real
                            // `ToolCallRequested`, so the pane header/
                            // transcript "preparing a tool call…" feedback is
                            // exercisable without a network provider.
                            let call_id = ToolCallId("mock-streaming-tool-1".to_string());
                            pending_tool_call = Some(call_id.clone());
                            for (bytes, tool_id) in [
                                (0usize, Some("mock.approval_required".to_string())),
                                (64, None),
                                (512, None),
                            ] {
                                let _ = events_tx.send(ProviderEvent::tool_call_progress(
                                    ToolCallProgress {
                                        key: call_id.0.clone(),
                                        tool_id,
                                        bytes,
                                    },
                                ));
                                thread::sleep(MOCK_STREAM_CHUNK_TICK);
                            }
                            let _ = events_tx.send(
                                Event::ToolCallRequested(ToolCallRequest {
                                    call_id,
                                    tool_id: "mock.approval_required".to_string(),
                                    input: serde_json::json!({ "message": text }),
                                })
                                .into(),
                            );
                            continue;
                        }
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
                        if lower_text.contains("bash") {
                            // Manual test hook for exercising the *real*
                            // `bash` approval/execution path (not just the
                            // approval-only `mock.approval_required` tool)
                            // without a network provider -- used by
                            // `horizon-sessiond`'s e2e suite to prove bash
                            // actually runs sessiond-side.
                            let call_id = ToolCallId("mock-bash-1".to_string());
                            pending_tool_call = Some(call_id.clone());
                            let _ = events_tx.send(
                                Event::ToolCallRequested(ToolCallRequest {
                                    call_id,
                                    tool_id: "bash".to_string(),
                                    input: serde_json::json!({ "command": "echo sessiond-bash-ok" }),
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
                            // Also the cheap, deterministic stand-in for the
                            // rig streaming path's provider-request lifecycle
                            // markers (see `Event::ProviderRequestSent`'s doc
                            // comment) so ordering is unit-testable without a
                            // network provider.
                            let _ = events_tx.send(
                                Event::ProviderRequestSent(ProviderRequestSent {
                                    model: "mock".to_string(),
                                })
                                .into(),
                            );
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
                            Event::ToolCallFinished(ToolCallResult::new(
                                call_id.clone(),
                                serde_json::json!({
                                    "approved": true,
                                    "result": "mock tool completed",
                                }),
                            ))
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
                            Event::ToolCallFinished(ToolCallResult::new(
                                call_id.clone(),
                                serde_json::json!({
                                    "approved": false,
                                    "reason": reason,
                                }),
                            ))
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
                    Command::ContinueTurn => {
                        // The mock provider has no turn-loop guard, so it
                        // never halts a turn in the first place -- a safe
                        // no-op, same spirit as `Command::Cancel`'s
                        // "nothing active" branch above.
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
    let mut first_token_sent = false;

    for word in full_response.split_inclusive(' ') {
        match commands_rx.recv_timeout(MOCK_STREAM_CHUNK_TICK) {
            Ok(Command::Cancel { .. }) => {
                let _ = events_tx.send(Event::ProviderRequestFinished.into());
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
                let _ = events_tx.send(Event::ProviderRequestFinished.into());
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

        if !first_token_sent {
            first_token_sent = true;
            let _ = events_tx.send(Event::ProviderRequestFirstToken.into());
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

    let _ = events_tx.send(Event::ProviderRequestFinished.into());
    let _ = events_tx.send(
        Event::MessageCommitted(Message {
            role: MessageRole::Assistant,
            text: streamed,
        })
        .into(),
    );
    MockTurnOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_model_is_none_regardless_of_role() {
        let provider = MockProvider::new();
        assert_eq!(Provider::resolved_model(&provider, None), None);
        assert_eq!(
            Provider::resolved_model(&provider, Some(&RoleId("config".to_string()))),
            None
        );
    }
}
