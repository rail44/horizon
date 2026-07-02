use std::path::PathBuf;

use floem::prelude::*;

use crate::agent_config::AgentConfig;
use crate::app::runtime::{spawn_agent_session, spawn_terminal_session};
use crate::commands::{command_enabled, CommandId};
use crate::control_surface::command_state;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{request_active_pane_focus, PaneFocusRequests, PaneKind, Workspace};

#[derive(Clone)]
pub(crate) struct CommandActionState {
    pub(crate) workspace: RwSignal<Workspace>,
    pub(crate) frames: RwSignal<Frames>,
    pub(crate) sessions: RwSignal<Registry>,
    pub(crate) pane_focus_requests: PaneFocusRequests,
    pub(crate) agent_state_status: RwSignal<Option<String>>,
    pub(crate) agent_config: AgentConfig,
    pub(crate) terminal_dump: Option<PathBuf>,
    pub(crate) clipboard_dump: Option<PathBuf>,
}

pub(crate) fn execute_command(command_id: CommandId, state: CommandActionState) {
    let workspace = state.workspace;
    let command_state = workspace.with_untracked(command_state);
    if !command_enabled(command_id, command_state) {
        return;
    }

    match command_id {
        CommandId::NewTerminal => open_tab(state, PaneKind::Terminal),
        CommandId::NewAgent => {
            open_tab(state, PaneKind::Agent);
        }
        CommandId::SplitActivePane => {
            split_active_pane(state);
        }
        CommandId::FocusNextPane => {
            workspace.update(Workspace::focus_next);
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandId::CloseActivePane => {
            workspace.update(|ws| {
                ws.close_active_pane();
            });
        }
        CommandId::CloseActiveTab => {
            workspace.update(|ws| {
                ws.close_active_tab();
            });
        }
        CommandId::TerminateActiveSession => {
            terminate_active_session(workspace, state.frames, state.sessions);
        }
    }
}

fn open_tab(state: CommandActionState, kind: PaneKind) {
    let workspace = state.workspace;
    let mut session_id = None;
    workspace.update(|ws| {
        session_id = Some(ws.open_tab_with_new_session(kind));
    });
    let session_id = session_id.expect("new session");
    spawn_session(kind, session_id, &state);
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

fn spawn_session(kind: PaneKind, session_id: SessionId, state: &CommandActionState) {
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

fn split_active_pane(state: CommandActionState) {
    let workspace = state.workspace;
    let mut split = None;
    workspace.update(|ws| {
        split = ws.split_active_with_new_session();
    });

    let Some((kind, session_id)) = split else {
        return;
    };
    spawn_session(kind, session_id, &state);
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

fn terminate_active_session(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    let mut terminated = None;
    workspace.update(|ws| {
        terminated = ws.terminate_active_session();
    });

    let Some(session_id) = terminated else {
        return;
    };
    sessions.update(|registry| {
        registry.shutdown_terminal(session_id);
        registry.shutdown_agent(session_id);
    });
    frames.update(|frames| frames.remove_session(session_id));
}
