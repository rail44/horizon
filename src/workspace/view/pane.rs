use crate::agent::contract::{Command, SessionState, ToolCallId};
use crate::agent::frame::AgentFrame;
use crate::agent::tools::{resolve_approval, ApprovalDecision, ApprovalOutcome};
use crate::app::keymap::is_palette_open_key;
use crate::control_surface::{
    handle_control_key, open_palette, ControlInputState, ControlMode, OpenPaletteState,
};
use crate::session::{Frames, Registry};
use crate::terminal::TerminalFrame;
use crate::ui::theme;
use crate::workspace::{
    handle_active_pane_key, visible_agent_sender, visible_terminal_sender, AgentDrafts, PaneKind,
    Workspace,
};
use floem::prelude::*;
use floem::reactive::create_effect;
use floem::{
    action::set_ime_allowed,
    event::{Event, EventListener, EventPropagation},
    peniko::kurbo::{Point, Size},
};

use super::agent_controls::{
    agent_approval_actions, agent_cancel_action, agent_composer, gate_pending_approval,
};
use super::chrome::pane_header;
use super::terminal_output::terminal_output;
use crate::agent::view as agent_view;

#[derive(Clone)]
pub(super) struct PaneViewState {
    pub(super) control_input: ControlInputState,
    pub(super) open_palette: OpenPaletteState,
    pub(super) ime_composing: RwSignal<bool>,
    pub(super) ime_preedit: RwSignal<Option<String>>,
    pub(super) ime_cursor_area: RwSignal<(Point, Size)>,
    pub(super) agent_drafts: AgentDrafts,
}

impl PaneViewState {
    fn control_input_state(&self) -> ControlInputState {
        self.control_input.clone()
    }

    fn open_palette_state(&self) -> OpenPaletteState {
        self.open_palette
    }
}

pub(super) fn pane_view(
    state: PaneViewState,
    index: usize,
    focus_request: RwSignal<u64>,
) -> impl IntoView {
    let control_input = state.control_input_state();
    let open_palette_state = state.open_palette_state();

    let workspace = control_input.command.workspace();
    let frames = control_input.command.frames();
    let sessions = control_input.command.sessions();
    let ime_composing = state.ime_composing;
    let ime_preedit = state.ime_preedit;
    let ime_cursor_area = state.ime_cursor_area;
    let palette_open = control_input.palette_open;
    let agent_drafts = state.agent_drafts;
    let control_mode = control_input.control_mode;

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
    // Set the instant the cancel action fires, cleared when the session
    // state next changes; while set, approve/deny are dead (see
    // `gate_pending_approval`) so an approval click can't race an
    // in-flight cancellation into executing the tool anyway.
    let cancel_requested = RwSignal::new(false);
    let agent_session_state = move || {
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        frames.with(|frames| frames.agent_frame(session_id).state)
    };
    create_effect(move |previous: Option<Option<SessionState>>| {
        let state = agent_session_state();
        if let Some(previous) = previous {
            if previous != state && cancel_requested.get_untracked() {
                cancel_requested.set(false);
            }
        }
        state
    });
    let pending_approval = move || {
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        let pending =
            frames.with(|frames| frames.agent_frame(session_id).pending_approval_call_id());
        gate_pending_approval(cancel_requested.get(), pending)
    };
    let turn_in_flight = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_agent_session_id(index)) else {
            return false;
        };
        frames.with(|frames| frames.agent_frame(session_id).is_turn_in_flight())
    };
    let agent_draft = agent_drafts[index];

    v_stack((
        pane_header(title, active, closeable, move || {
            workspace.update(|ws| {
                ws.close_visible_pane(index);
            });
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
                resolve_and_send_approval(
                    workspace,
                    frames,
                    sessions,
                    index,
                    call_id,
                    ApprovalDecision::Approve,
                )
            },
            move |call_id| {
                resolve_and_send_approval(
                    workspace,
                    frames,
                    sessions,
                    index,
                    call_id,
                    ApprovalDecision::Deny {
                        reason: Some("Denied by user".to_string()),
                    },
                )
            },
        ),
        agent_cancel_action(is_agent, turn_in_flight, move || {
            cancel_requested.set(true);
            if let Some(tx) = visible_agent_sender(workspace, sessions, index) {
                let _ = tx.send(Command::Cancel { request_id: None });
            }
        }),
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
            if palette_open.get_untracked() && handle_control_key(key_event, control_input.clone())
            {
                return EventPropagation::Stop;
            }

            if is_palette_open_key(key_event) {
                ime_composing.set(false);
                ime_preedit.set(None);
                set_ime_allowed(false);
                control_mode.set(ControlMode::Commands);
                open_palette(open_palette_state);
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

/// Resolves the user's approve/deny click for the visible pane's pending
/// tool call and sends the resulting command to the provider.
///
/// `fs.write`/`fs.edit`/`bash` are executed by Horizon itself on approval —
/// see `agent::tools::resolve_approval` — so this also folds the execution
/// (or, for `bash`, the running-state) result into the session's frame
/// before sending. Every other approval-gated tool (e.g.
/// `mock.approval_required`) falls back to forwarding `ApproveToolCall`/
/// `DenyToolCall` to the provider unchanged.
fn resolve_and_send_approval(
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    index: usize,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) {
    let Some(session_id) = workspace.with_untracked(|ws| ws.visible_agent_session_id(index)) else {
        return;
    };
    let frame = frames.with_untracked(|frames| frames.agent_frame(session_id));
    let command = match resolve_approval(&frame, session_id, call_id, decision) {
        ApprovalOutcome::Executed { frame, command } => {
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
            command
        }
        // `bash` on approve: the running-state frame is ready to publish,
        // but the tool is executing off the UI thread and hasn't produced a
        // result yet. Nothing to send to the provider here — the eventual
        // `Command::ToolCallResult` is sent later by the effect
        // `app/runtime/agent.rs::spawn_agent_session` sets up for it.
        ApprovalOutcome::Started { frame } => {
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
            return;
        }
        ApprovalOutcome::Forward(command) => command,
        // Duplicate click on a call that already resolved (double-approve,
        // or approve racing an earlier result) — nothing to run or send.
        ApprovalOutcome::AlreadyResolved => return,
    };
    if let Some(tx) = visible_agent_sender(workspace, sessions, index) {
        let _ = tx.send(command);
    }
}
