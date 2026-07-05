mod agent;
mod terminal;

use std::path::PathBuf;

use floem::prelude::*;

use crate::agent::agentd_runtime::AgentdConnection;
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
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agentd_connection: RwSignal<Option<AgentdConnection>>,
}

impl SessionRuntimeState {
    pub(crate) fn new(
        workspace: RwSignal<Workspace>,
        frames: RwSignal<Frames>,
        sessions: RwSignal<Registry>,
        agent_state_status: RwSignal<Option<String>>,
        terminal_dump: Option<PathBuf>,
        clipboard_dump: Option<PathBuf>,
        agentd_connection: RwSignal<Option<AgentdConnection>>,
    ) -> Self {
        Self {
            workspace,
            frames,
            sessions,
            agent_state_status,
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

    pub(crate) fn agent_state_status(&self) -> RwSignal<Option<String>> {
        self.agent_state_status
    }

    pub(crate) fn agentd_connection(&self) -> RwSignal<Option<AgentdConnection>> {
        self.agentd_connection
    }
}

/// Flushes any runtime state that buffers writes in memory before the app
/// exits normally. A no-op since step 4: Horizon no longer owns any
/// buffered writer itself -- the agent event log moved entirely into
/// `horizon-agentd` in step 3, and step 4 retired the in-process agent
/// runtime that used to open a copy of it here. Kept (rather than removed)
/// so `app::shutdown`/`main.rs`'s `AppEvent::WillTerminate` wiring doesn't
/// need to change; terminal sessions have no buffered-write concern of
/// their own either.
pub(crate) fn shutdown() {}

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
            state.frames,
            state.sessions,
            state.agent_state_status,
            state.agentd_connection,
        ),
    }
}
