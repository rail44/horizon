use std::path::PathBuf;

use crate::agent_config::AgentConfig;
use crate::app::commands::{CommandActionState, PaneFocusRequests};
use crate::control_surface::ControlMode;
use crate::input::palette_accepts_text_input;
use crate::session::Frames;
use crate::session::Registry;
use crate::workspace::Workspace;
use floem::keyboard::{Key, KeyEvent, NamedKey};
use floem::prelude::*;

use crate::control_surface::actions::{
    close_control_surface, close_palette, execute_overview_selection, execute_palette_selection,
    move_overview_selection, move_palette_selection, update_palette_query, OverviewActionState,
    PaletteActionState,
};

#[derive(Clone)]
pub struct ControlInputState {
    pub workspace: RwSignal<Workspace>,
    pub frames: RwSignal<Frames>,
    pub sessions: RwSignal<Registry>,
    pub palette_open: RwSignal<bool>,
    pub palette_query: RwSignal<String>,
    pub palette_selection: RwSignal<usize>,
    pub control_mode: RwSignal<ControlMode>,
    pub overview_selection: RwSignal<usize>,
    pub pane_focus_requests: PaneFocusRequests,
    pub agent_state_status: RwSignal<Option<String>>,
    pub agent_config: AgentConfig,
    pub terminal_dump: Option<PathBuf>,
    pub clipboard_dump: Option<PathBuf>,
}

#[derive(Clone)]
pub(crate) struct WorkspaceControlState {
    pub(crate) workspace: RwSignal<Workspace>,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) control_mode: RwSignal<ControlMode>,
    pub(crate) overview_selection: RwSignal<usize>,
}

fn handle_palette_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_palette(state.palette_open, state.palette_query);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_palette_selection(palette_action_state(state));
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_palette_selection(
                state.workspace,
                state.palette_query,
                state.palette_selection,
                -1,
            );
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_palette_selection(
                state.workspace,
                state.palette_query,
                state.palette_selection,
                1,
            );
            true
        }
        Key::Named(NamedKey::Backspace) => {
            update_palette_query(
                state.workspace,
                state.palette_query,
                state.palette_selection,
                |query| {
                    query.pop();
                },
            );
            true
        }
        Key::Named(NamedKey::Space) => {
            update_palette_query(
                state.workspace,
                state.palette_query,
                state.palette_selection,
                |query| {
                    query.push(' ');
                },
            );
            true
        }
        Key::Character(text) if palette_accepts_text_input(key_event.modifiers) => {
            update_palette_query(
                state.workspace,
                state.palette_query,
                state.palette_selection,
                |query| {
                    query.push_str(text.as_str());
                },
            );
            true
        }
        _ => false,
    }
}

fn palette_action_state(state: ControlInputState) -> PaletteActionState {
    PaletteActionState {
        command: CommandActionState {
            workspace: state.workspace,
            frames: state.frames,
            sessions: state.sessions,
            pane_focus_requests: state.pane_focus_requests,
            agent_state_status: state.agent_state_status,
            agent_config: state.agent_config,
            terminal_dump: state.terminal_dump,
            clipboard_dump: state.clipboard_dump,
        },
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
    }
}

pub fn handle_control_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(state.control_mode);
        return true;
    }

    match state.control_mode.get_untracked() {
        ControlMode::Commands => handle_palette_key(key_event, state),
        ControlMode::Workspace => {
            handle_workspace_control_key(key_event, workspace_control_state(&state))
        }
    }
}

fn workspace_control_state(state: &ControlInputState) -> WorkspaceControlState {
    WorkspaceControlState {
        workspace: state.workspace,
        palette_open: state.palette_open,
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
    }
}

pub(crate) fn handle_workspace_control_key(
    key_event: &KeyEvent,
    state: WorkspaceControlState,
) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(state.control_mode);
        return true;
    }

    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_control_surface(state.palette_open);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_overview_selection(overview_action_state(&state));
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_overview_selection(state.workspace, state.overview_selection, -1);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_overview_selection(state.workspace, state.overview_selection, 1);
            true
        }
        _ => false,
    }
}

fn overview_action_state(state: &WorkspaceControlState) -> OverviewActionState {
    OverviewActionState {
        workspace: state.workspace,
        palette_open: state.palette_open,
        overview_selection: state.overview_selection,
    }
}

fn is_control_mode_switch_key(event: &KeyEvent) -> bool {
    matches!(event.key.logical_key, Key::Named(NamedKey::Tab))
}

fn switch_control_mode(control_mode: RwSignal<ControlMode>) {
    control_mode.update(|mode| {
        *mode = match *mode {
            ControlMode::Commands => ControlMode::Workspace,
            ControlMode::Workspace => ControlMode::Commands,
        };
    });
}
