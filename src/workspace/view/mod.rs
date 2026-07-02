use std::path::PathBuf;

use crate::agent_config::AgentConfig;
use crate::app::commands::PaneFocusRequests;
use crate::control_surface::ControlMode;
use crate::session::Frames;
use crate::session::Registry;
use crate::ui::theme;
use crate::workspace::{AgentDrafts, Workspace};
use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

mod agent_controls;
mod chrome;
mod pane;
mod tab_strip;
mod terminal_output;

use pane::PaneViewState;
pub use tab_strip::tab_strip;

#[derive(Clone)]
pub struct WorkspaceViewState {
    pub workspace: RwSignal<Workspace>,
    pub frames: RwSignal<Frames>,
    pub sessions: RwSignal<Registry>,
    pub ime_composing: RwSignal<bool>,
    pub ime_preedit: RwSignal<Option<String>>,
    pub ime_cursor_area: RwSignal<(Point, Size)>,
    pub palette_open: RwSignal<bool>,
    pub palette_query: RwSignal<String>,
    pub palette_selection: RwSignal<usize>,
    pub palette_focus_request: RwSignal<u64>,
    pub pane_focus_requests: PaneFocusRequests,
    pub agent_drafts: AgentDrafts,
    pub agent_config: AgentConfig,
    pub control_mode: RwSignal<ControlMode>,
    pub overview_selection: RwSignal<usize>,
    pub terminal_dump: Option<PathBuf>,
    pub clipboard_dump: Option<PathBuf>,
    pub agent_state_status: RwSignal<Option<String>>,
}

pub fn workspace_view(state: WorkspaceViewState) -> impl IntoView {
    let pane_focus_requests = state.pane_focus_requests;
    let pane_state = PaneViewState {
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
        pane_focus_requests,
        agent_drafts: state.agent_drafts,
        agent_config: state.agent_config,
        control_mode: state.control_mode,
        overview_selection: state.overview_selection,
        terminal_dump: state.terminal_dump,
        clipboard_dump: state.clipboard_dump,
        agent_state_status: state.agent_state_status,
    };

    h_stack((
        pane::pane_view(pane_state.clone(), 0, pane_focus_requests[0]),
        pane::pane_view(pane_state.clone(), 1, pane_focus_requests[1]),
        pane::pane_view(pane_state.clone(), 2, pane_focus_requests[2]),
        pane::pane_view(pane_state, 3, pane_focus_requests[3]),
    ))
    .style(|s| {
        s.flex()
            .flex_row()
            .width_full()
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .gap(1)
            .padding(1)
            .background(theme::border_subtle())
    })
}
