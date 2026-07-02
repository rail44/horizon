use std::path::PathBuf;

use crate::agent_config::AgentConfig;
use crate::control_surface::ControlMode;
use crate::session::Frames;
use crate::session::Registry;
use crate::ui::theme;
use crate::workspace::{AgentDrafts, PaneFocusRequests, Workspace};
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

impl WorkspaceViewState {
    fn pane_view_state(&self) -> PaneViewState {
        PaneViewState {
            workspace: self.workspace,
            frames: self.frames,
            sessions: self.sessions,
            ime_composing: self.ime_composing,
            ime_preedit: self.ime_preedit,
            ime_cursor_area: self.ime_cursor_area,
            palette_open: self.palette_open,
            palette_query: self.palette_query,
            palette_selection: self.palette_selection,
            palette_focus_request: self.palette_focus_request,
            pane_focus_requests: self.pane_focus_requests,
            agent_drafts: self.agent_drafts,
            agent_config: self.agent_config.clone(),
            control_mode: self.control_mode,
            overview_selection: self.overview_selection,
            terminal_dump: self.terminal_dump.clone(),
            clipboard_dump: self.clipboard_dump.clone(),
            agent_state_status: self.agent_state_status,
        }
    }
}

pub fn workspace_view(state: WorkspaceViewState) -> impl IntoView {
    let pane_focus_requests = state.pane_focus_requests;
    let pane_state = state.pane_view_state();
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
