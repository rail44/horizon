mod agent;
mod terminal;

use std::path::PathBuf;

use floem::prelude::*;
use horizon_agent::roles::RoleId;

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
    config_reload_requests: RwSignal<u64>,
}

impl SessionRuntimeState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        workspace: RwSignal<Workspace>,
        frames: RwSignal<Frames>,
        sessions: RwSignal<Registry>,
        agent_state_status: RwSignal<Option<String>>,
        terminal_dump: Option<PathBuf>,
        clipboard_dump: Option<PathBuf>,
        agentd_connection: RwSignal<Option<AgentdConnection>>,
        config_reload_requests: RwSignal<u64>,
    ) -> Self {
        Self {
            workspace,
            frames,
            sessions,
            agent_state_status,
            terminal_dump,
            clipboard_dump,
            agentd_connection,
            config_reload_requests,
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

    /// Bumped by `agent::agentd_runtime`'s `config.write` observation
    /// (`fold_agent_session_events`); the app view answers a bump by
    /// executing the `Reload Config` command -- see `app::view`.
    pub(crate) fn config_reload_requests(&self) -> RwSignal<u64> {
        self.config_reload_requests
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

/// `role_id` selects a role-tagged agent session (`horizon_agent::roles`);
/// it only means something for `PaneKind::Agent` -- a terminal spawn
/// ignores it, and every caller but the `New Configuration Agent` command
/// passes `None` today.
pub(crate) fn spawn_session(
    kind: PaneKind,
    role_id: Option<RoleId>,
    session_id: SessionId,
    state: &SessionRuntimeState,
) {
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
            role_id,
            state.frames,
            state.sessions,
            state.agent_state_status,
            state.agentd_connection,
            state.config_reload_requests,
        ),
    }
}
