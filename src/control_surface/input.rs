use crate::app::command_actions::CommandActionState;
use crate::app::keymap::palette_accepts_text_input;
use floem::keyboard::{Key, KeyEvent, NamedKey};
use floem::prelude::*;

use crate::control_surface::actions::{
    close_palette, execute_palette_selection, move_palette_selection, update_palette_query,
    PaletteActionState,
};

#[derive(Clone)]
pub(crate) struct ControlInputState {
    pub(crate) command: CommandActionState,
    pub(crate) palette_open: RwSignal<bool>,
    pub(crate) palette_query: RwSignal<String>,
    pub(crate) palette_selection: RwSignal<usize>,
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
}

/// `Event::KeyDown` entry point for the command palette -- the control
/// surface is Commands-only now that the Tab-switching workspace overview
/// is gone (`docs/plans/application-ui/01-session-manager.md`; session
/// management moved to its own modal, `control_surface::view::
/// session_manager`).
pub(crate) fn handle_control_key(key_event: &KeyEvent, state: ControlInputState) -> bool {
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
