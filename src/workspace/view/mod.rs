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

pub fn workspace_view(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    palette_open: RwSignal<bool>,
    palette_query: RwSignal<String>,
    palette_selection: RwSignal<usize>,
    palette_focus_request: RwSignal<u64>,
    pane_focus_requests: PaneFocusRequests,
    agent_drafts: AgentDrafts,
    agent_config: AgentConfig,
    control_mode: RwSignal<ControlMode>,
    overview_selection: RwSignal<usize>,
    terminal_dump: Option<PathBuf>,
    clipboard_dump: Option<PathBuf>,
    agent_state_status: RwSignal<Option<String>>,
) -> impl IntoView {
    let pane_state = PaneViewState {
        workspace,
        frames,
        sessions,
        ime_composing,
        ime_preedit,
        ime_cursor_area,
        palette_open,
        palette_query,
        palette_selection,
        palette_focus_request,
        pane_focus_requests,
        agent_drafts,
        agent_config,
        control_mode,
        overview_selection,
        terminal_dump,
        clipboard_dump,
        agent_state_status,
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
