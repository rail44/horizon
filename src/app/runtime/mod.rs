mod agent;
mod terminal;

use std::path::PathBuf;

use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::agent::config::AgentConfig;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{PaneKind, Workspace};

use agent::spawn_agent_session;
use terminal::spawn_terminal_session;

#[derive(Clone)]
pub(crate) struct SessionRuntimeState {
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agentd_connection: Option<AgentdConnection>,
}

impl SessionRuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        workspace: RwSignal<Workspace>,
        frames: RwSignal<Frames>,
        sessions: RwSignal<Registry>,
        agent_state_status: RwSignal<Option<String>>,
        agent_config: AgentConfig,
        terminal_dump: Option<PathBuf>,
        clipboard_dump: Option<PathBuf>,
        agentd_connection: Option<AgentdConnection>,
    ) -> Self {
        Self {
            workspace,
            frames,
            sessions,
            agent_state_status,
            agent_config,
            terminal_dump,
            clipboard_dump,
            agentd_connection,
        }
    }

    pub(crate) fn workspace(&self) -> RwSignal<Workspace> {
        self.workspace
    }

    pub(crate) fn frames(&self) -> RwSignal<Frames> {
        self.frames
    }

    pub(crate) fn sessions(&self) -> RwSignal<Registry> {
        self.sessions
    }
}

/// Flushes any runtime state that buffers writes in memory before the app
/// exits normally. Currently that's just the agent event log's
/// process-global writer thread (see `agent::shutdown_agent_event_log` and
/// the design comment on `agent::AGENT_EVENT_LOG_WRITER`); terminal
/// sessions have no equivalent buffered-write concern today.
pub(crate) fn shutdown() {
    agent::shutdown_agent_event_log();
}

pub(crate) fn spawn_session(kind: PaneKind, session_id: SessionId, state: &SessionRuntimeState) {
    match kind {
        PaneKind::Terminal => spawn_terminal_session(
            session_id,
            state.frames,
            state.sessions,
            state.terminal_dump.clone(),
            state.clipboard_dump.clone(),
        ),
        PaneKind::Agent => spawn_agent_session(
            session_id,
            state.workspace,
            state.frames,
            state.sessions,
            state.agent_state_status,
            state.agent_config.clone(),
            state.agentd_connection.as_ref(),
        ),
    }
}
