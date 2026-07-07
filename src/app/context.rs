use crate::app::command_actions::CommandActionState;
use crate::control_surface::view::CommandPaletteState;
use crate::control_surface::{ControlInputState, OpenPaletteState, SessionManagerHandle};
use crate::workspace::view::WorkspaceViewState;

use super::state::AppState;

pub(super) fn workspace_view_state(state: &AppState) -> WorkspaceViewState {
    WorkspaceViewState {
        control_input: control_input_state(state),
        open_palette: open_palette_state(state),
        ime_composing: state.ime_composing,
        ime_preedit: state.ime_preedit,
        ime_cursor_area: state.ime_cursor_area,
        agent_drafts: state.agent_drafts,
    }
}

pub(super) fn command_palette_state(state: &AppState) -> CommandPaletteState {
    CommandPaletteState {
        control_input: control_input_state(state),
        palette_focus_request: state.palette_focus_request,
    }
}

pub(super) fn open_palette_state(state: &AppState) -> OpenPaletteState {
    OpenPaletteState {
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        palette_stage: state.palette_stage,
        palette_focus_request: state.palette_focus_request,
    }
}

pub(super) fn control_input_state(state: &AppState) -> ControlInputState {
    ControlInputState {
        command: command_action_state(state),
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
    }
}

pub(super) fn command_action_state(state: &AppState) -> CommandActionState {
    CommandActionState {
        runtime: state.session_runtime_state(),
        pane_focus_requests: state.pane_focus_requests,
        session_manager: session_manager_handle(state),
        palette: open_palette_state(state),
    }
}

/// The session manager modal's signals, bundled for `CommandActionState`
/// (see `control_surface::SessionManagerHandle`'s doc comment) -- also
/// reused directly by `app::input::AppInput`'s own `CommandActionState`
/// constructor.
pub(super) fn session_manager_handle(state: &AppState) -> SessionManagerHandle {
    SessionManagerHandle {
        open: state.session_manager_open,
        selection: state.session_manager_selection,
        pending_terminate: state.session_manager_pending_terminate,
        focus_request: state.session_manager_focus_request,
    }
}
