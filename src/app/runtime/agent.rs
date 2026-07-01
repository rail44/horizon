use std::sync::{Mutex, OnceLock};

use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use crate::agent::persistence::event_log::{read, WriterHandle};
use crate::agent::persistence::projection::duckdb::Store;
use crate::agent::tools::process_agent_provider_event;
use crate::agent::{contract as agent, frame::AgentFrame, live::LiveState};
use crate::agent_config::{AgentConfig, AgentPersistenceConfig};
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::Workspace;

static AGENT_EVENT_LOG_WRITER: OnceLock<Mutex<Option<WriterHandle>>> = OnceLock::new();
static AGENT_DUCKDB_REBUILD_DONE: OnceLock<Mutex<bool>> = OnceLock::new();

pub fn spawn_agent_session(
    session_id: SessionId,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
) {
    let providers = agent::ProviderRegistry::builtin_with_config(agent_config.clone());
    let provider_id = providers.default_provider_id();
    let runtime_state = open_agent_runtime_state_store(
        session_id,
        provider_id.clone(),
        agent_state_status,
        &agent_config.persistence,
    );
    let Some(handle) = providers.start_session(&provider_id, session_id) else {
        frames.update(|frames| {
            frames.update_agent_frame(
                session_id,
                AgentFrame {
                    state: None,
                    items: Vec::new(),
                },
            );
        });
        return;
    };
    let events = create_signal_from_channel(handle.events());
    sessions.update(|registry| {
        registry.insert_agent(session_id, handle);
    });

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::Initialize(agent::Initialization {
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
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
        }
    });
}

fn open_agent_runtime_state_store(
    session_id: SessionId,
    provider_id: agent::ProviderId,
    agent_state_status: RwSignal<Option<String>>,
    persistence_config: &AgentPersistenceConfig,
) -> LiveState {
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
        LiveState::with_event_log(session_id, Some(provider_id), event_log)
    } else {
        LiveState::with_disabled_persistence()
    }
}

fn open_agent_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<(WriterHandle, Option<String>)> {
    let writer_cell = AGENT_EVENT_LOG_WRITER.get_or_init(|| Mutex::new(None));
    let mut writer = writer_cell
        .lock()
        .map_err(|_| anyhow::anyhow!("agent event log writer lock poisoned"))?;
    if let Some(writer) = writer.as_ref() {
        return Ok((writer.clone(), None));
    }

    let path = persistence_config.event_log_path.clone();
    let status = Some(format!("Agent event log: {}", path.display()));
    let handle = WriterHandle::open(path)?;
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

    let report = read(&log_path)?;

    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let store = Store::open(db_path)?;
    store.replace_from_event_log_records(report.records)?;
    Ok(())
}
