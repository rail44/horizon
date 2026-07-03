use crate::agent::prompt::{system_prompt, SessionEnvironment};
use crate::agent::{contract as agent, frame::*, policy::horizon_events_for_provider_event};
use crate::session::SessionId;

fn recv_event(rx: &crossbeam_channel::Receiver<agent::ProviderEvent>) -> agent::ProviderEvent {
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .expect("expected a provider event within timeout")
}

#[test]
fn mock_agent_emits_initial_session_events() {
    let provider = crate::agent::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
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
    let store = crate::agent::live::LiveState::new();
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

#[test]
fn runtime_state_store_enqueues_events_to_jsonl_log() {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-runtime-log-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let session_id = SessionId::new();
    let provider_id = agent::ProviderId("builtin.agent.rig".to_string());
    let writer =
        crate::agent::persistence::event_log::WriterHandle::open(&path).expect("event log");
    let store = crate::agent::live::LiveState::with_event_log(
        session_id,
        Some(provider_id.clone()),
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
    writer.flush_for_tests().expect("flush");

    let report = crate::agent::persistence::event_log::read(&path).expect("read log");
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
    let call_id = agent::ToolCallId("call-1".to_string());
    let mut frame = AgentFrame::empty();
    frame
        .items
        .push(AgentFrameItem::ApprovalRequested(agent::ApprovalRequest {
            call_id: call_id.clone(),
            reason: "needs approval".to_string(),
        }));

    assert_eq!(frame.pending_approval_call_id(), Some(call_id.clone()));

    frame
        .items
        .push(AgentFrameItem::ToolCallFinished(agent::ToolCallResult {
            call_id,
            output: serde_json::json!({ "ok": true }),
        }));

    assert_eq!(frame.pending_approval_call_id(), None);
}

#[test]
fn horizon_policy_adds_approval_for_requested_tool() {
    let call_id = agent::ToolCallId("call-1".to_string());
    let events = horizon_events_for_provider_event(&agent::Event::ToolCallRequested(
        agent::ToolCallRequest {
            call_id: call_id.clone(),
            tool_id: "mock.approval_required".to_string(),
            input: serde_json::json!({}),
        },
    ));

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
    let provider = crate::agent::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    let _ = tx.send(agent::Command::ToolCallResult(agent::ToolCallResult {
        call_id: agent::ToolCallId("call-1".to_string()),
        output: serde_json::json!({ "ok": true }),
    }));

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
    let provider = crate::agent::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
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

    // Cancel as soon as the first streamed chunk arrives, well before the
    // mock's simulated turn would finish on its own.
    assert!(matches!(
        recv_event(&rx).event,
        agent::Event::AssistantTextDelta(_)
    ));
    let _ = tx.send(agent::Command::Cancel { request_id: None });

    let mut partial_commit = None;
    let mut saw_cancelled_state = false;
    loop {
        match recv_event(&rx).event {
            agent::Event::AssistantTextDelta(_) => {}
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
    let partial = partial_commit.expect("partial assistant text committed on cancel");
    assert!(!partial.is_empty());
    let full_response = "Mock response: slow please";
    assert!(
        full_response.starts_with(&partial) && partial != full_response,
        "expected a strict partial prefix of {full_response:?}, got {partial:?}"
    );
}

#[test]
fn mock_agent_cancel_marks_pending_approval_cancelled_and_recovers() {
    let provider = crate::agent::providers::mock::MockProvider::new();
    let handle = agent::Provider::start_session(
        &provider,
        agent::StartSession {
            session_id: SessionId::new(),
            provider_id: agent::Provider::provider_id(&provider),
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
    let _ = tx.send(agent::Command::ToolCallResult(agent::ToolCallResult {
        call_id,
        output: serde_json::json!({ "ignored": true }),
    }));
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

    let prompt = system_prompt(&environment);

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

    let prompt = system_prompt(&environment);

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

    let line_count = system_prompt(&environment).lines().count();

    assert!(
        line_count <= LINE_BUDGET,
        "system prompt grew to {line_count} lines, budget is {LINE_BUDGET}"
    );
}

#[test]
fn system_prompt_carries_tool_policy_and_retry_nudge() {
    let prompt = system_prompt(&SessionEnvironment {
        cwd: std::path::PathBuf::from("/repo"),
        os: "linux",
        git_repo: true,
    });

    let lower = prompt.to_ascii_lowercase();
    assert!(lower.contains("absolute path"));
    assert!(lower.contains("retry"));
}

#[test]
fn system_prompt_carries_destructive_action_caution() {
    let prompt = system_prompt(&SessionEnvironment {
        cwd: std::path::PathBuf::from("/repo"),
        os: "linux",
        git_repo: true,
    });

    assert!(prompt.to_ascii_lowercase().contains("destructive"));
}

#[test]
fn current_environment_reports_this_process_cwd() {
    let environment = SessionEnvironment::current();

    assert_eq!(environment.cwd, std::env::current_dir().unwrap());
    assert_eq!(environment.os, std::env::consts::OS);
}

#[test]
fn provider_registry_starts_builtin_provider() {
    let registry = agent::ProviderRegistry::builtin();
    let provider_id = registry.default_provider_id();
    let handle = registry
        .start_session(&provider_id, SessionId::new())
        .expect("builtin provider");

    let first = handle.events().recv().expect("first event");
    assert_eq!(
        first.event,
        agent::Event::StateChanged(agent::SessionState::Created)
    );
}
