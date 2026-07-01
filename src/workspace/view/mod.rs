use std::path::PathBuf;

use crate::agent_config::AgentConfig;
use crate::app::commands::{active_agent, PaneFocusRequests, MAX_VISIBLE_PANES};
use crate::control_surface::ControlMode;
use crate::session::Frames;
use crate::session::Registry;
use crate::terminal::TerminalCommand;
use crate::workspace::Workspace;
use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

mod agent_controls;
mod chrome;
mod pane;
mod tab_strip;
mod terminal_output;

pub use tab_strip::tab_strip;

pub type AgentDrafts = [RwSignal<String>; MAX_VISIBLE_PANES];

pub fn active_agent_draft(
    workspace: RwSignal<Workspace>,
    agent_drafts: AgentDrafts,
) -> Option<RwSignal<String>> {
    if !active_agent(workspace) {
        return None;
    }

    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    agent_drafts.get(index).copied()
}

pub fn active_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.active_terminal_session_id())?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}

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
    h_stack((
        pane::pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            0,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[0],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane::pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            1,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[1],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane::pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            2,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[2],
            pane_focus_requests,
            agent_drafts,
            agent_config.clone(),
            control_mode,
            overview_selection,
            terminal_dump.clone(),
            clipboard_dump.clone(),
            agent_state_status,
        ),
        pane::pane_view(
            workspace,
            frames,
            sessions,
            ime_composing,
            ime_preedit,
            ime_cursor_area,
            3,
            palette_open,
            palette_query,
            palette_selection,
            palette_focus_request,
            pane_focus_requests[3],
            pane_focus_requests,
            agent_drafts,
            agent_config,
            control_mode,
            overview_selection,
            terminal_dump,
            clipboard_dump,
            agent_state_status,
        ),
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
            .background(floem::peniko::Color::rgb8(42, 46, 55))
    })
}
