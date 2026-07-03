use std::sync::{Mutex, OnceLock};

use crossbeam_channel::unbounded;
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use crate::agent::config::{AgentConfig, AgentPersistenceConfig};
use crate::agent::persistence::event_log::{read, WriterHandle};
use crate::agent::persistence::projection::duckdb::Store;
use crate::agent::tools::{
    process_agent_provider_event, register_session_runtime, should_fold_completion, BashCompletion,
    ToolSessionState,
};
use crate::agent::{contract as agent, frame::AgentFrame, live::LiveState};
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::Workspace;

static AGENT_EVENT_LOG_WRITER: OnceLock<Mutex<Option<WriterHandle>>> = OnceLock::new();
static AGENT_DUCKDB_REBUILD_DONE: OnceLock<Mutex<bool>> = OnceLock::new();

pub(super) fn spawn_agent_session(
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

    let tool_state = ToolSessionState::for_current_dir();
    // `bash` calls run on a dedicated background thread and can take up to
    // their timeout, so their result can't fold into `LiveState`/`Frames`
    // synchronously the way `fs.write`/`fs.edit` do (those are UI-thread-
    // confined — see `agent::live::LiveState`). This channel is the
    // cross-thread-to-UI delivery seam for that result: the same
    // `crossbeam_channel` + `create_signal_from_channel` bridge used just
    // above for `handle.events()`, the provider's own event stream.
    let (bash_results_tx, bash_results_rx) = unbounded::<BashCompletion>();
    let bash_completions = create_signal_from_channel(bash_results_rx);
    register_session_runtime(
        session_id,
        tool_state.clone(),
        runtime_state.clone(),
        bash_results_tx,
    );

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::Initialize(agent::Initialization {
            session_id,
            provider_id: provider_id.clone(),
        }));
    }

    let bash_runtime_state = runtime_state.clone();
    create_effect(move |_| {
        if let Some(event) = events.get() {
            let processing =
                workspace.with_untracked(|ws| process_agent_provider_event(ws, &tool_state, event));
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

    create_effect(move |_| {
        if let Some(completion) = bash_completions.get() {
            fold_bash_completion(
                &bash_runtime_state,
                frames,
                sessions,
                session_id,
                completion,
            );
        }
    });
}

/// Folds a finished bash call's result into the session's frame and
/// forwards it to the provider — the async-execution analogue of
/// `agent::tools::approval::ApprovalOutcome::Executed`'s synchronous fold,
/// which the click handler in `workspace/view/pane.rs` does directly.
///
/// Guards against double-folding a late result the same way `agent::tools::
/// approval` guards a double approve/deny: if the call already has a
/// `ToolCallFinished` — because a turn cancellation raced this completion
/// and got there first (see `agent::tools::processing::
/// process_agent_provider_event`, which kills the child but can't stop a
/// result that was already in flight on this channel) — the result is
/// accepted and discarded here, matching the provider's own contract for a
/// `ToolCallResult` arriving after cancel.
fn fold_bash_completion(
    runtime_state: &LiveState,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    session_id: SessionId,
    completion: BashCompletion,
) {
    let result = completion.result;
    if !should_fold_completion(&runtime_state.frame(), &result.call_id) {
        return;
    }

    let events = [
        agent::Event::ToolCallFinished(result.clone()),
        agent::Event::StateChanged(agent::SessionState::WaitingForUser),
    ];
    let frame = runtime_state.extend_provider_events(events.into_iter().map(Into::into));
    frames.update(|frames| frames.update_agent_frame(session_id, frame));

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::ToolCallResult(result));
    }
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
