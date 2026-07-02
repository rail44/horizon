use std::path::PathBuf;

use crate::agent::contract::Command;
use crate::agent::frame::AgentFrame;
use crate::agent_config::AgentConfig;
use crate::app::commands::{close_visible_pane, PaneFocusRequests};
use crate::control_surface::{handle_control_key, open_palette, ControlInputState, ControlMode};
use crate::input::is_palette_open_key;
use crate::session::{Frames, Registry};
use crate::terminal::TerminalFrame;
use crate::ui::theme;
use crate::workspace::{
    handle_active_pane_key, visible_agent_sender, visible_terminal_sender, AgentDrafts, PaneKind,
    Workspace,
};
use floem::prelude::*;
use floem::{
    action::set_ime_allowed,
    event::{Event, EventListener, EventPropagation},
    peniko::kurbo::{Point, Size},
};

use super::agent_controls::{agent_approval_actions, agent_composer};
use super::chrome::pane_header;
use super::terminal_output::terminal_output;
use crate::agent::view as agent_view;

#[derive(Clone)]
pub(super) struct PaneViewState {
    pub(super) workspace: RwSignal<Workspace>,
    pub(super) frames: RwSignal<Frames>,
    pub(super) sessions: RwSignal<Registry>,
    pub(super) ime_composing: RwSignal<bool>,
    pub(super) ime_preedit: RwSignal<Option<String>>,
    pub(super) ime_cursor_area: RwSignal<(Point, Size)>,
    pub(super) palette_open: RwSignal<bool>,
    pub(super) palette_query: RwSignal<String>,
    pub(super) palette_selection: RwSignal<usize>,
    pub(super) palette_focus_request: RwSignal<u64>,
    pub(super) pane_focus_requests: PaneFocusRequests,
    pub(super) agent_drafts: AgentDrafts,
    pub(super) agent_config: AgentConfig,
    pub(super) control_mode: RwSignal<ControlMode>,
    pub(super) overview_selection: RwSignal<usize>,
    pub(super) terminal_dump: Option<PathBuf>,
    pub(super) clipboard_dump: Option<PathBuf>,
    pub(super) agent_state_status: RwSignal<Option<String>>,
}

pub(super) fn pane_view(
    state: PaneViewState,
    index: usize,
    focus_request: RwSignal<u64>,
) -> impl IntoView {
    let workspace = state.workspace;
    let frames = state.frames;
    let sessions = state.sessions;
    let ime_composing = state.ime_composing;
    let ime_preedit = state.ime_preedit;
    let ime_cursor_area = state.ime_cursor_area;
    let palette_open = state.palette_open;
    let palette_query = state.palette_query;
    let palette_selection = state.palette_selection;
    let palette_focus_request = state.palette_focus_request;
    let pane_focus_requests = state.pane_focus_requests;
    let agent_drafts = state.agent_drafts;
    let agent_config = state.agent_config;
    let control_mode = state.control_mode;
    let overview_selection = state.overview_selection;
    let terminal_dump = state.terminal_dump;
    let clipboard_dump = state.clipboard_dump;
    let agent_state_status = state.agent_state_status;
    let control_input = ControlInputState {
        workspace,
        frames,
        sessions,
        palette_open,
        palette_query,
        palette_selection,
        control_mode,
        overview_selection,
        pane_focus_requests,
        agent_state_status,
        agent_config,
        terminal_dump,
        clipboard_dump,
    };

    let terminal_frame = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_terminal_session_id(index)) else {
            return TerminalFrame::from_text("No split yet".to_string());
        };
        frames.with(|frames| frames.terminal_frame(session_id))
    };
    let agent_frame = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_agent_session_id(index)) else {
            return AgentFrame::empty();
        };
        frames.with(|frames| frames.agent_frame(session_id))
    };

    let title = move || {
        workspace.with(|ws| {
            ws.visible_pane_title(index)
                .unwrap_or_else(|| "Empty".to_string())
        })
    };

    let active = move || workspace.with(|ws| ws.active_visible_index() == index);
    let exists = move || workspace.with(|ws| ws.visible_pane_kind(index).is_some());
    let closeable = move || workspace.with(|ws| ws.visible_panes().len() > 1);
    let is_agent =
        move || workspace.with(|ws| ws.visible_pane_kind(index) == Some(PaneKind::Agent));
    let is_terminal =
        move || workspace.with(|ws| ws.visible_pane_kind(index) == Some(PaneKind::Terminal));
    let pending_approval = move || {
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        frames.with(|frames| frames.agent_frame(session_id).pending_approval_call_id())
    };
    let agent_draft = agent_drafts[index];

    v_stack((
        pane_header(title, active, closeable, move || {
            close_visible_pane(workspace, sessions, index)
        }),
        terminal_output(
            terminal_frame,
            move || {
                if active() {
                    ime_preedit.get()
                } else {
                    None
                }
            },
            visible_terminal_sender(workspace, sessions, index),
            ime_cursor_area,
            is_terminal,
        ),
        agent_view::agent_frame_view(agent_frame, is_agent),
        agent_approval_actions(
            is_agent,
            pending_approval,
            move |call_id| {
                if let Some(tx) = visible_agent_sender(workspace, sessions, index) {
                    let _ = tx.send(Command::ApproveToolCall { call_id });
                }
            },
            move |call_id| {
                if let Some(tx) = visible_agent_sender(workspace, sessions, index) {
                    let _ = tx.send(Command::DenyToolCall {
                        call_id,
                        reason: Some("Denied by user".to_string()),
                    });
                }
            },
        ),
        agent_composer(
            is_agent,
            active,
            agent_draft,
            move || {
                if active() && is_agent() {
                    ime_preedit.get()
                } else {
                    None
                }
            },
            ime_cursor_area,
        ),
    ))
    .style(|s| {
        s.flex()
            .flex_col()
            .size_full()
            .min_width(0.0)
            .justify_start()
    })
    .keyboard_navigable()
    .request_focus(move || {
        focus_request.get();
    })
    .on_event(EventListener::PointerDown, move |_| {
        focus_request.update(|request| *request += 1);
        workspace.update(|ws| {
            ws.activate_visible_pane(index);
        });
        if workspace.with(|ws| ws.active_visible_pane_accepts_text_input(index)) {
            set_ime_allowed(true);
        }
        EventPropagation::Stop
    })
    .on_event(EventListener::FocusGained, move |_| {
        set_ime_allowed(workspace.with(|ws| ws.active_visible_pane_accepts_text_input(index)));
        EventPropagation::Continue
    })
    .on_event(EventListener::FocusLost, move |_| {
        ime_composing.set(false);
        ime_preedit.set(None);
        set_ime_allowed(false);
        EventPropagation::Continue
    })
    .on_event(EventListener::KeyDown, move |event| {
        if let Event::KeyDown(key_event) = event {
            if palette_open.get_untracked() {
                if handle_control_key(key_event, control_input.clone()) {
                    return EventPropagation::Stop;
                }
            }

            if is_palette_open_key(key_event) {
                ime_composing.set(false);
                ime_preedit.set(None);
                set_ime_allowed(false);
                control_mode.set(ControlMode::Commands);
                open_palette(
                    palette_open,
                    palette_query,
                    palette_selection,
                    palette_focus_request,
                );
                return EventPropagation::Stop;
            }
        }

        if let Event::KeyDown(key_event) = event {
            if handle_active_pane_key(
                key_event,
                workspace,
                sessions,
                index,
                ime_composing,
                agent_draft,
            ) {
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    })
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let border = if active() {
            theme::accent()
        } else {
            theme::surface_selected()
        };
        s.height_full()
            .min_width(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .background(theme::surface_panel())
            .border(1.0)
            .border_color(border)
    })
}
