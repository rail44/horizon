use std::{path::PathBuf, thread};

use crossbeam_channel::unbounded;
use rig_core::completion::Message;

use crate::{
    agent::config::RigAgentConfig,
    agent::contract::{
        Command, Event, Message as AgentMessage, MessageRole, ProviderEvent, SessionHandle,
        SessionState, StartSession, ToolCallResult,
    },
};

use super::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
    load_rig_history, rig_initialization_message, rig_tool_result_message,
};

pub(super) fn spawn_rig_session(
    request: StartSession,
    config: RigAgentConfig,
    memory_duckdb_path: Option<PathBuf>,
) -> SessionHandle {
    let (commands_tx, commands_rx) = unbounded();
    let (events_tx, events_rx) = unbounded::<ProviderEvent>();
    let provider_id = request.provider_id;
    let session_id = request.session_id;

    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().ok();
        let mut rig_history = load_rig_history(memory_duckdb_path.as_deref(), session_id);

        let _ = events_tx.send(Event::StateChanged(SessionState::Created).into());
        let _ = events_tx.send(
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::Assistant,
                text: rig_initialization_message(&provider_id, &config, rig_history.len()),
            })
            .into(),
        );
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());

        while let Ok(command) = commands_rx.recv() {
            match command {
                Command::Initialize(_) => {
                    let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                    let _ =
                        events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
                }
                Command::UserMessage { text } => {
                    handle_user_message(&runtime, &config, &mut rig_history, &events_tx, text);
                }
                Command::ToolCallResult(result) => {
                    handle_tool_result(&runtime, &config, &mut rig_history, &events_tx, result);
                }
                Command::Shutdown => {
                    let _ = events_tx.send(Event::StateChanged(SessionState::Terminated).into());
                    break;
                }
                Command::Cancel { .. }
                | Command::ApproveToolCall { .. }
                | Command::DenyToolCall { .. } => {}
            }
        }
    });

    SessionHandle::new(commands_tx, events_rx)
}

fn handle_user_message(
    runtime: &Option<tokio::runtime::Runtime>,
    config: &RigAgentConfig,
    rig_history: &mut Vec<Message>,
    events_tx: &crossbeam_channel::Sender<ProviderEvent>,
    text: String,
) {
    let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
    let _ = events_tx.send(
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: text.clone(),
        })
        .into(),
    );

    let contains_tool_call = complete_rig_turn(
        runtime.as_ref(),
        config,
        rig_history,
        Message::user(text.clone()),
        events_tx,
        || deterministic_rig_response(&text),
    );
    if !contains_tool_call {
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
    }
}

fn handle_tool_result(
    runtime: &Option<tokio::runtime::Runtime>,
    config: &RigAgentConfig,
    rig_history: &mut Vec<Message>,
    events_tx: &crossbeam_channel::Sender<ProviderEvent>,
    result: ToolCallResult,
) {
    let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
    let contains_tool_call = complete_rig_turn(
        runtime.as_ref(),
        config,
        rig_history,
        rig_tool_result_message(&result),
        events_tx,
        || deterministic_tool_result_response(&result),
    );
    if !contains_tool_call {
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
    }
}
