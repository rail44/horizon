use std::path::PathBuf;

use floem::action::set_ime_allowed;
use floem::prelude::*;

use crate::agent_config::AgentConfig;
use crate::app::runtime::{spawn_agent_session, spawn_terminal_session};
use crate::commands::{command_enabled, CommandId};
use crate::control_surface::command_state;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{PaneKind, Workspace};

pub const MAX_VISIBLE_PANES: usize = 4;

pub type PaneFocusRequests = [RwSignal<u64>; MAX_VISIBLE_PANES];

#[derive(Clone)]
pub struct CommandActionState {
    pub workspace: RwSignal<Workspace>,
    pub frames: RwSignal<Frames>,
    pub sessions: RwSignal<Registry>,
    pub pane_focus_requests: PaneFocusRequests,
    pub agent_state_status: RwSignal<Option<String>>,
    pub agent_config: AgentConfig,
    pub terminal_dump: Option<PathBuf>,
    pub clipboard_dump: Option<PathBuf>,
}

pub fn execute_command(command_id: CommandId, state: CommandActionState) {
    let workspace = state.workspace;
    let command_state = workspace.with_untracked(command_state);
    if !command_enabled(command_id, command_state) {
        return;
    }

    match command_id {
        CommandId::NewTerminal => open_terminal_tab(state),
        CommandId::NewAgent => {
            open_agent_tab(state);
        }
        CommandId::SplitActivePane => {
            split_active_pane(state);
        }
        CommandId::FocusNextPane => {
            workspace.update(Workspace::focus_next);
            request_active_pane_focus(workspace, state.pane_focus_requests);
        }
        CommandId::CloseActivePane => {
            let index = workspace.with_untracked(|ws| ws.active_visible_index());
            close_visible_pane(workspace, state.sessions, index);
        }
        CommandId::CloseActiveTab => {
            let index = workspace.with_untracked(|ws| ws.active_tab_index());
            close_tab(workspace, state.sessions, index);
        }
        CommandId::TerminateActiveSession => {
            terminate_active_session(workspace, state.frames, state.sessions);
        }
    }
}

pub fn open_terminal_tab(state: CommandActionState) {
    let session_id = SessionId::new();
    let workspace = state.workspace;
    workspace.update(|ws| {
        ws.open_tab(PaneKind::Terminal, Some(session_id));
    });
    spawn_terminal_session(
        session_id,
        state.frames,
        state.sessions,
        state.terminal_dump,
        state.clipboard_dump,
    );
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

pub fn open_agent_tab(state: CommandActionState) {
    let session_id = SessionId::new();
    let workspace = state.workspace;
    workspace.update(|ws| {
        ws.open_tab(PaneKind::Agent, Some(session_id));
    });
    spawn_agent_session(
        session_id,
        workspace,
        state.frames,
        state.sessions,
        state.agent_state_status,
        state.agent_config,
    );
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

pub fn split_active_pane(state: CommandActionState) {
    let workspace = state.workspace;
    let kind = workspace.with_untracked(|ws| {
        ws.active_terminal_session_id()
            .map(|_| PaneKind::Terminal)
            .unwrap_or(PaneKind::Agent)
    });
    workspace.update(|ws| {
        if kind == PaneKind::Terminal {
            ws.split_active(PaneKind::Terminal, Some(SessionId::new()));
        } else {
            ws.split_active(PaneKind::Agent, Some(SessionId::new()));
        }
    });
    if kind == PaneKind::Terminal {
        let Some(session_id) = workspace.with_untracked(|ws| ws.active_terminal_session_id())
        else {
            return;
        };
        spawn_terminal_session(
            session_id,
            state.frames,
            state.sessions,
            state.terminal_dump,
            state.clipboard_dump,
        );
    } else if let Some(session_id) = workspace.with_untracked(|ws| ws.active_session_id()) {
        spawn_agent_session(
            session_id,
            workspace,
            state.frames,
            state.sessions,
            state.agent_state_status,
            state.agent_config,
        );
    }
    request_active_pane_focus(workspace, state.pane_focus_requests);
}

pub fn request_active_pane_focus(
    workspace: RwSignal<Workspace>,
    pane_focus_requests: PaneFocusRequests,
) {
    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    if let Some(focus_request) = pane_focus_requests.get(index) {
        focus_request.update(|request| *request += 1);
    }
    set_ime_allowed(active_text_input_pane(workspace));
}

pub fn terminate_active_session(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    let Some(session_id) = workspace.with_untracked(|ws| ws.active_session_id()) else {
        return;
    };

    workspace.update(|ws| {
        ws.terminate_session(session_id);
    });
    sessions.update(|registry| {
        registry.shutdown_terminal(session_id);
        registry.shutdown_agent(session_id);
    });
    frames.update(|frames| frames.remove_session(session_id));
}

pub fn close_visible_pane(
    workspace: RwSignal<Workspace>,
    _sessions: RwSignal<Registry>,
    index: usize,
) {
    workspace.update(|ws| {
        ws.close_visible_pane(index);
    });
}

pub fn close_tab(workspace: RwSignal<Workspace>, _sessions: RwSignal<Registry>, index: usize) {
    workspace.update(|ws| {
        ws.close_tab_index(index);
    });
}

pub fn active_terminal(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_is(PaneKind::Terminal))
}

pub fn active_agent(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_is(PaneKind::Agent))
}

pub fn active_text_input_pane(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_accepts_text_input())
}
