use crate::control_surface::{ControlInputState, OpenPaletteState};
use crate::ui::theme;
use crate::workspace::AgentDrafts;
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
    pub control_input: ControlInputState,
    pub open_palette: OpenPaletteState,
    pub ime_composing: RwSignal<bool>,
    pub ime_preedit: RwSignal<Option<String>>,
    pub ime_cursor_area: RwSignal<(Point, Size)>,
    pub agent_drafts: AgentDrafts,
}

impl WorkspaceViewState {
    fn pane_view_state(&self) -> PaneViewState {
        PaneViewState {
            control_input: self.control_input.clone(),
            open_palette: self.open_palette,
            ime_composing: self.ime_composing,
            ime_preedit: self.ime_preedit,
            ime_cursor_area: self.ime_cursor_area,
            agent_drafts: self.agent_drafts,
        }
    }
}

pub fn workspace_view(state: WorkspaceViewState) -> impl IntoView {
    let pane_focus_requests = state.control_input.command.pane_focus_requests;
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
