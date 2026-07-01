use std::path::PathBuf;

use floem::keyboard::{Key, KeyEvent, NamedKey};
use floem::prelude::*;
use horizon::agent_config::AgentConfig;
use horizon::app::commands::PaneFocusRequests;
use horizon::control_surface::ControlMode;
use horizon::input::palette_accepts_text_input;
use horizon::session::Frames;
use horizon::session::Registry;
use horizon::workspace::Workspace;

use super::actions::{
    close_control_surface, close_palette, execute_overview_selection, execute_palette_selection,
    move_overview_selection, move_palette_selection, update_palette_query,
};

fn handle_palette_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> bool {
    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_palette(palette_open, palette_query);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_palette_selection(
                workspace,
                frames,
                sessions,
                palette_open,
                palette_query,
                palette_selection,
                pane_focus_requests,
                agent_state_status,
                agent_config,
                terminal_dump,
                clipboard_dump,
            );
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_palette_selection(workspace, palette_query, palette_selection, -1);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_palette_selection(workspace, palette_query, palette_selection, 1);
            true
        }
        Key::Named(NamedKey::Backspace) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.pop();
            });
            true
        }
        Key::Named(NamedKey::Space) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.push(' ');
            });
            true
        }
        Key::Character(text) if palette_accepts_text_input(key_event.modifiers) => {
            update_palette_query(workspace, palette_query, palette_selection, |query| {
                query.push_str(text.as_str());
            });
            true
        }
        _ => false,
    }
}

pub(crate) fn handle_control_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    pane_focus_requests: PaneFocusRequests,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(control_mode);
        return true;
    }

    match control_mode.get_untracked() {
        ControlMode::Commands => handle_palette_key(
            key_event,
            workspace,
            frames,
            sessions,
            palette_open,
            palette_query,
            palette_selection,
            pane_focus_requests,
            agent_state_status,
            agent_config,
            terminal_dump,
            clipboard_dump,
        ),
        ControlMode::Workspace => handle_workspace_control_key(
            key_event,
            workspace,
            palette_open,
            control_mode,
            overview_selection,
        ),
    }
}

pub(super) fn handle_workspace_control_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    palette_open: RwSignal<bool>,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
) -> bool {
    if is_control_mode_switch_key(key_event) {
        switch_control_mode(control_mode);
        return true;
    }

    match &key_event.key.logical_key {
        Key::Named(NamedKey::Escape) => {
            close_control_surface(palette_open);
            true
        }
        Key::Named(NamedKey::Enter) => {
            execute_overview_selection(workspace, palette_open, overview_selection);
            true
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_overview_selection(workspace, overview_selection, -1);
            true
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_overview_selection(workspace, overview_selection, 1);
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
