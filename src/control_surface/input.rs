use crate::app::command_actions::CommandActionState;
use crate::app::keymap::palette_accepts_text_input;
use crate::control_surface::ControlMode;
use crate::workspace::Workspace;
use floem::keyboard::{Key, KeyEvent, NamedKey};
use floem::prelude::*;

use crate::control_surface::actions::{
    close_control_surface, close_palette, execute_overview_selection, execute_palette_selection,
    move_overview_selection, move_palette_selection, update_palette_query, OverviewActionState,
    PaletteActionState,
};

#[derive(Clone)]
pub(crate) struct ControlInputState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
    pub(crate) control_mode: RwSignal<ControlMode>,
    pub(crate) overview_selection: RwSignal<usize>,
}

impl ControlInputState {
    pub(crate) fn palette_action_state(self) -> PaletteActionState {
        PaletteActionState {
            command: self.command,
            palette_open: self.palette_open,
            palette_query: self.palette_query,
            palette_selection: self.palette_selection,
        }
    }

    pub(crate) fn workspace_control_state(&self) -> WorkspaceControlState {
        WorkspaceControlState {
            workspace: self.command.workspace(),
            palette_open: self.palette_open,
            control_mode: self.control_mode,
            overview_selection: self.overview_selection,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WorkspaceControlState {
    pub(crate) workspace: RwSignal<Workspace>,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) control_mode: RwSignal<ControlMode>,
    pub(crate) overview_selection: RwSignal<usize>,
}

impl WorkspaceControlState {
    pub(crate) fn overview_action_state(&self) -> OverviewActionState {
        OverviewActionState {
            workspace: self.workspace,
            palette_open: self.palette_open,
            overview_selection: self.overview_selection,
        }
    }
}

fn handle_palette_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
    let workspace = state.command.workspace();
    let frames = state.command.frames();
    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_palette(state.palette_open, state.palette_query);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_palette_selection(state.palette_action_state());
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_palette_selection(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                -1,
            );
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_palette_selection(
                workspace,
                frames,
                state.palette_query,
                state.palette_selection,
                1,
            );
            true
        }
        Key::Named(NamedKey::Backspace) => {
            update_palette_query(
                workspace,
                frames,
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
                workspace,
                frames,
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
                workspace,
                frames,
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

pub(crate) fn handle_control_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(state.control_mode);
        return true;
    }

    match state.control_mode.get_untracked() {
        ControlMode::Commands => handle_palette_key(key_event, state),
        ControlMode::Workspace => {
            handle_workspace_control_key(key_event, state.workspace_control_state())
        }
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
            execute_overview_selection(state.overview_action_state());
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
