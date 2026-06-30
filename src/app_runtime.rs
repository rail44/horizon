use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;
use floem::Clipboard;

use crate::agent::{
    AgentCommand, AgentFrame, AgentInitialization, AgentProviderId, AgentProviderRegistry,
    AgentRuntimeStateStore,
};
use crate::agent_config::{AgentConfig, AgentPersistenceConfig};
use crate::agent_duckdb_state::DuckDbAgentStateStore;
use crate::agent_event_log::{read_agent_event_log, AgentEventLogWriterHandle};
use crate::agent_tools::process_agent_provider_event;
use crate::session::SessionRegistry;
use crate::terminal::{TerminalSession, TerminalSize, TerminalUpdate};
use crate::workspace::{SessionId, Workspace};

static AGENT_EVENT_LOG_WRITER: OnceLock<Mutex<Option<AgentEventLogWriterHandle>>> = OnceLock::new();
static AGENT_DUCKDB_REBUILD_DONE: OnceLock<Mutex<bool>> = OnceLock::new();

pub fn spawn_terminal_session(
    session_id: SessionId,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) {
    match TerminalSession::spawn(TerminalSize::default()) {
        Ok(session) => {
            sessions.update(|registry| {
                registry.insert_terminal(session_id, session.sender());
            });
            let updates = create_signal_from_channel(session.updates());
            create_effect(move |_| {
                if let Some(update) = updates.get() {
                    match update {
                        TerminalUpdate::Snapshot(output) => {
                            if let Some(path) = &terminal_dump {
                                let _ = std::fs::write(path, &output.text);
                            }
                            workspace.update(|ws| ws.update_terminal_frame(session_id, output));
                        }
                        TerminalUpdate::Error(error) => {
                            workspace.update(|ws| {
                                ws.update_terminal_output(
                                    session_id,
                                    format!("Terminal error: {error}"),
                                )
                            });
                        }
                        TerminalUpdate::Exited => {
                            workspace.update(|ws| {
                                ws.update_terminal_output(session_id, "Terminal exited".to_string())
                            });
                        }
                        TerminalUpdate::Title(_) | TerminalUpdate::Bell => {}
                        TerminalUpdate::Clipboard(text) => {
                            if let Some(path) = &clipboard_dump {
                                let _ = std::fs::write(path, &text);
                            }
                            let _ = Clipboard::set_contents(text);
                        }
                    }
                }
            });
        }
        Err(error) => {
            workspace.update(|ws| {
                ws.update_terminal_output(session_id, format!("Terminal error: {error}"))
            });
        }
    }
}

pub fn spawn_agent_session(
    session_id: SessionId,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<SessionRegistry>,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
) {
    let providers = AgentProviderRegistry::builtin_with_config(agent_config.clone());
    let provider_id = providers.default_provider_id();
    let runtime_state = open_agent_runtime_state_store(
        session_id,
        provider_id.clone(),
        agent_state_status,
        &agent_config.persistence,
    );
    let Some(handle) = providers.start_session(&provider_id, session_id) else {
        workspace.update(|ws| {
            ws.update_agent_frame(
                session_id,
                AgentFrame {
                    state: None,
                    items: Vec::new(),
                },
            )
        });
        return;
    };
    let events = create_signal_from_channel(handle.events());
    sessions.update(|registry| {
        registry.insert_agent(session_id, handle);
    });

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(AgentCommand::Initialize(AgentInitialization {
            session_id,
            provider_id: provider_id.clone(),
        }));
    }

    create_effect(move |_| {
        if let Some(event) = events.get() {
            let processing = workspace.with_untracked(|ws| process_agent_provider_event(ws, event));
            for command in processing.provider_commands {
                if let Some(sender) =
                    sessions.with_untracked(|registry| registry.agent_sender(session_id))
                {
                    let _ = sender.send(command);
                }
            }
            let frame = runtime_state.extend_provider_events(processing.horizon_events);
            workspace.update(|ws| ws.update_agent_frame(session_id, frame));
        }
    });
}

fn open_agent_runtime_state_store(
    session_id: SessionId,
    provider_id: AgentProviderId,
    agent_state_status: RwSignal<Option<String>>,
    persistence_config: &AgentPersistenceConfig,
) -> AgentRuntimeStateStore {
    let event_log = match open_agent_event_log(persistence_config) {
        Ok((writer, status)) => {
            let mut messages = Vec::new();
            if let Some(status) = status {
                messages.push(status);
            }
            if let Err(error) = rebuild_agent_duckdb_from_event_log_once(persistence_config) {
                messages.push(format!(
                    "Agent DuckDB projection rebuild unavailable: {error}"
                ));
            }
            if !messages.is_empty() {
                agent_state_status.set(Some(messages.join(" | ")));
            }
            Some(writer)
        }
        Err(error) => {
            agent_state_status.set(Some(format!(
                "Agent event log unavailable ({error}); persistence disabled"
            )));
            None
        }
    };

    if let Some(event_log) = event_log {
        AgentRuntimeStateStore::with_event_log(session_id, Some(provider_id), event_log)
    } else {
        AgentRuntimeStateStore::with_disabled_persistence()
    }
}

fn open_agent_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<(AgentEventLogWriterHandle, Option<String>)> {
    let writer_cell = AGENT_EVENT_LOG_WRITER.get_or_init(|| Mutex::new(None));
    let mut writer = writer_cell
        .lock()
        .map_err(|_| anyhow::anyhow!("agent event log writer lock poisoned"))?;
    if let Some(writer) = writer.as_ref() {
        return Ok((writer.clone(), None));
    }

    let path = persistence_config.event_log_path.clone();
    let status = Some(format!("Agent event log: {}", path.display()));
    let handle = AgentEventLogWriterHandle::open(path)?;
    *writer = Some(handle.clone());
    Ok((handle, status))
}

fn rebuild_agent_duckdb_from_event_log_once(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<()> {
    let rebuild_done = AGENT_DUCKDB_REBUILD_DONE.get_or_init(|| Mutex::new(false));
    let mut rebuild_done = rebuild_done
        .lock()
        .map_err(|_| anyhow::anyhow!("agent DuckDB rebuild lock poisoned"))?;
    if *rebuild_done {
        return Ok(());
    }

    rebuild_agent_duckdb_from_event_log(persistence_config)?;
    *rebuild_done = true;
    Ok(())
}

fn rebuild_agent_duckdb_from_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<()> {
    let Some(db_path) = persistence_config.duckdb_path.clone() else {
        return Ok(());
    };
    let log_path = persistence_config.event_log_path.clone();

    let report = read_agent_event_log(&log_path)?;

    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let store = DuckDbAgentStateStore::open(db_path)?;
    store.replace_from_event_log_records(report.records)?;
    Ok(())
}
