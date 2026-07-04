use crate::app::command_actions::CommandActionState;
use crate::control_surface::view::{CommandPaletteState, WorkspaceOverviewState};
use crate::control_surface::{ControlInputState, OpenPaletteState};
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

pub(super) fn workspace_overview_state(state: &AppState) -> WorkspaceOverviewState {
    WorkspaceOverviewState {
        workspace_control: control_input_state(state).workspace_control_state(),
        palette_focus_request: state.palette_focus_request,
    }
}

pub(super) fn open_palette_state(state: &AppState) -> OpenPaletteState {
    OpenPaletteState {
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        palette_focus_request: state.palette_focus_request,
    }
}

pub(super) fn control_input_state(state: &AppState) -> ControlInputState {
    ControlInputState {
        command: command_action_state(state),
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
    }
}

pub(super) fn command_action_state(state: &AppState) -> CommandActionState {
    CommandActionState {
        runtime: state.session_runtime_state(),
        pane_focus_requests: state.pane_focus_requests,
    }
}
