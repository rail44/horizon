use crate::control_surface::{ControlInputState, OpenPaletteState};
use crate::workspace::AgentDrafts;
use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;
use floem::reactive::create_effect;

mod agent_controls;
mod chrome;
mod composer_text;
mod layout_tree;
mod pane;
mod tab_strip;
mod terminal_output;

use pane::PaneViewState;
pub(crate) use tab_strip::tab_strip;

#[derive(Clone)]
pub(crate) struct WorkspaceViewState {
    pub(crate) control_input: ControlInputState,
    pub(crate) open_palette: OpenPaletteState,
    pub(crate) ime_composing: RwSignal<bool>,
    pub(crate) ime_preedit: RwSignal<Option<String>>,
    pub(crate) ime_cursor_area: RwSignal<(Point, Size)>,
    pub(crate) agent_drafts: AgentDrafts,
}

impl WorkspaceViewState {
    fn pane_view_state(&self) -> PaneViewState {
        PaneViewState {
            control_input: self.control_input.clone(),
            open_palette: self.open_palette,
            ime_composing: self.ime_composing,
            ime_preedit: self.ime_preedit,
            ime_cursor_area: self.ime_cursor_area,
            agent_drafts: self.agent_drafts.clone(),
        }
    }
}

pub(crate) fn workspace_view(state: WorkspaceViewState) -> impl IntoView {
    let workspace = state.control_input.command.workspace();
    let pane_focus_requests = state.control_input.command.pane_focus_requests.clone();
    let agent_drafts = state.agent_drafts.clone();
    // Prunes both `PaneId`-keyed per-pane UI maps (`workspace::input::
    // PaneKeyedSignals`) against every pane that still exists anywhere in
    // the workspace -- not just the active tab's visible ones, so a
    // background tab's drafts survive a tab switch. Runs once at mount and
    // again on every workspace mutation, which covers every pane-removal
    // path (the pane header's close button, `CloseActivePane`/`CloseTab`,
    // terminate, the CLI control plane) uniformly, without needing to
    // thread cleanup through each one individually.
    create_effect(move |_| {
        let live = workspace.with(|ws| ws.all_pane_ids());
        pane_focus_requests.retain(&live);
        agent_drafts.retain(&live);
    });

    let pane_state = state.pane_view_state();
    layout_tree::layout_tree_view(pane_state).style(|s| {
        s.flex()
            .width_full()
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
    })
}
