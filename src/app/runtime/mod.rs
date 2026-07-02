mod agent;
mod terminal;

use std::path::PathBuf;

use floem::prelude::*;

use crate::agent_config::AgentConfig;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{PaneKind, Workspace};

pub use agent::spawn_agent_session;
pub use terminal::spawn_terminal_session;

#[derive(Clone)]
pub(crate) struct SessionRuntimeState {
    pub(crate) workspace: RwSignal<Workspace>,
    pub(crate) frames: RwSignal<Frames>,
    pub(crate) sessions: RwSignal<Registry>,
    pub(crate) agent_state_status: RwSignal<Option<String>>,
    pub(crate) agent_config: AgentConfig,
    pub(crate) terminal_dump: Option<PathBuf>,
    pub(crate) clipboard_dump: Option<PathBuf>,
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
        ),
    }
}
