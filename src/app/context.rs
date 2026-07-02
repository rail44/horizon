use crate::control_surface::view::{CommandPaletteState, WorkspaceOverviewState};
use crate::control_surface::{ControlInputState, OpenPaletteState};
use crate::workspace::view::WorkspaceViewState;

use super::state::AppState;

pub(super) fn workspace_view_state(state: &AppState) -> WorkspaceViewState {
    WorkspaceViewState {
        workspace: state.workspace,
        frames: state.frames,
        sessions: state.sessions,
        ime_composing: state.ime_composing,
        ime_preedit: state.ime_preedit,
        ime_cursor_area: state.ime_cursor_area,
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        palette_focus_request: state.palette_focus_request,
        pane_focus_requests: state.pane_focus_requests,
        agent_drafts: state.agent_drafts,
        agent_config: state.agent_config.clone(),
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
        terminal_dump: state.terminal_dump.clone(),
        clipboard_dump: state.clipboard_dump.clone(),
        agent_state_status: state.agent_state_status,
    }
}

pub(super) fn command_palette_state(state: &AppState) -> CommandPaletteState {
    CommandPaletteState {
        workspace: state.workspace,
        frames: state.frames,
        sessions: state.sessions,
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        palette_focus_request: state.palette_focus_request,
        pane_focus_requests: state.pane_focus_requests,
        agent_state_status: state.agent_state_status,
        agent_config: state.agent_config.clone(),
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
        terminal_dump: state.terminal_dump.clone(),
        clipboard_dump: state.clipboard_dump.clone(),
    }
}

pub(super) fn workspace_overview_state(state: &AppState) -> WorkspaceOverviewState {
    WorkspaceOverviewState {
        workspace: state.workspace,
        palette_open: state.palette_open,
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
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
        workspace: state.workspace,
        frames: state.frames,
        sessions: state.sessions,
        palette_open: state.palette_open,
        palette_query: state.palette_query,
        palette_selection: state.palette_selection,
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
        pane_focus_requests: state.pane_focus_requests,
        agent_state_status: state.agent_state_status,
        agent_config: state.agent_config.clone(),
        terminal_dump: state.terminal_dump.clone(),
        clipboard_dump: state.clipboard_dump.clone(),
    }
}
