use crate::agent::contract::{SessionState, ToolCallId};
use crate::agent::frame::AgentFrame;
use crate::app::command_actions::{
    execute_command, CommandActionState, CommandInvocation, DEFAULT_DENY_REASON,
};
use crate::app::keymap::{is_palette_open_key, is_workspace_mode_enter_key};
use crate::control_surface::{
    handle_control_key, open_palette, ControlInputState, ControlMode, OpenPaletteState,
};
use crate::terminal::TerminalFrame;
use crate::ui::theme;
use crate::workspace::{
    agent_escape_requests_workspace_mode, handle_active_pane_key, handle_active_pane_key_release,
    handle_agent_banner_key, handle_workspace_mode_key, visible_terminal_sender, AgentDrafts,
    BannerKeyAction, ModeAction, PaneKind, Workspace,
};
use floem::prelude::*;
use floem::reactive::create_effect;
use floem::{
    action::set_ime_allowed,
    event::{Event, EventListener, EventPropagation},
    peniko::kurbo::{Point, Size},
};

use super::agent_controls::{
    agent_approval_banner, agent_composer, agent_pane_status_label, awaiting_call,
    describe_pending_call, gate_pending_approval, next_agent_pane_focus, next_answered_call,
    AgentPaneFocus,
};
use super::chrome::{pane_header, workspace_mode_scrim};
use super::terminal_output::terminal_output;
use crate::agent::view as agent_view;

/// Approves `call_id` on the agent session currently occupying visible pane
/// `index`, if any -- shared by the approval banner's `y`/click paths (see
/// `pane_view`'s `KeyDown` handler and its `agent_approval_banner` call) so
/// both dispatch through the exact same `CommandInvocation`.
fn approve_pending(
    workspace: RwSignal<Workspace>,
    command_state: CommandActionState,
    index: usize,
    call_id: ToolCallId,
) {
    let Some(session_id) = workspace.with_untracked(|ws| ws.visible_agent_session_id(index)) else {
        return;
    };
    execute_command(
        CommandInvocation::ApproveToolCall {
            session_id,
            call_id,
        },
        command_state,
    );
}

/// `n`/click counterpart to [`approve_pending`].
fn deny_pending(
    workspace: RwSignal<Workspace>,
    command_state: CommandActionState,
    index: usize,
    call_id: ToolCallId,
) {
    let Some(session_id) = workspace.with_untracked(|ws| ws.visible_agent_session_id(index)) else {
        return;
    };
    execute_command(
        CommandInvocation::DenyToolCall {
            session_id,
            call_id,
            reason: Some(DEFAULT_DENY_REASON.to_string()),
        },
        command_state,
    );
}

/// Optimistically answers `call_id` locally (the fix for the 2026-07
/// repeated-approval OOM incident -- see `agent_controls::agent_approval_
/// banner`'s doc comment) before actually dispatching the decision: marks
/// it answered so the banner can't be re-triggered for the same call
/// regardless of round-trip latency, releases pane-internal keyboard focus
/// back to the message box, then runs `dispatch`. Shared by the banner's
/// button clicks and the `y`/`n` key handler below so both go through the
/// exact same sequencing. Callers must gate on `awaiting_call` themselves
/// before calling this -- it doesn't re-check, it just performs the "lock
/// it in" side effects unconditionally.
fn answer_pending(
    answered_call: RwSignal<Option<ToolCallId>>,
    agent_pane_focus: RwSignal<AgentPaneFocus>,
    call_id: ToolCallId,
    dispatch: impl FnOnce(ToolCallId),
) {
    answered_call.set(Some(call_id.clone()));
    agent_pane_focus.set(AgentPaneFocus::MessageBox);
    dispatch(call_id);
}

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
    let command_state = control_input.command.clone();

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
    let workspace_mode_active = move || workspace.with(|ws| ws.is_workspace_mode_active());
    let is_cursor = move || {
        workspace.with(|ws| ws.is_workspace_mode_active() && ws.cursor_visible_index() == index)
    };
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
    let pending_approval_extra = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_agent_session_id(index)) else {
            return 0;
        };
        frames
            .with(|frames| {
                frames
                    .agent_frame(session_id)
                    .pending_approval_call_ids()
                    .len()
            })
            .saturating_sub(1)
    };
    // What's being approved (`docs/agent-tools-design.md`'s "Show what is
    // being approved"): the pending call's own request, rendered by
    // `describe_pending_call` (the command line for `bash`, the target path
    // for `fs.write`/`fs.edit`) -- shown next to the banner's key hints.
    let pending_summary = move || {
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        let call_id = pending_approval()?;
        frames.with(|frames| {
            frames
                .agent_frame(session_id)
                .tool_call_request(&call_id)
                .map(describe_pending_call)
        })
    };
    // Which of the pane's two key-focus targets is live right now -- the
    // message-box composer, or the approval banner (see `AgentPaneFocus`).
    // Driven entirely by `pending_approval` transitions (`next_agent_pane_
    // focus`): the banner grabs focus the instant a new call becomes the
    // oldest pending one and releases it the instant none remain, however
    // the resolution happened (`y`/`n`, a button click, or the palette all
    // converge on the same `pending_approval` signal). `Esc` sets this
    // directly (see the `KeyDown` handler below) without going through the
    // pending-approval signal at all, so it survives an unrelated refresh.
    let agent_pane_focus = RwSignal::new(AgentPaneFocus::default());
    // The pane's locally-recorded "this call was already answered" marker
    // (the optimistic banner-state fix -- see `agent_controls::
    // agent_approval_banner`'s doc comment): set the instant a keypress or
    // click answers a call (`answer_pending`), cleared here the instant the
    // resolution round-trips back (`next_answered_call`, driven by the same
    // `pending_approval` transitions the focus effect above reacts to, so
    // both update from the one effect below).
    let answered_call = RwSignal::<Option<ToolCallId>>::new(None);
    create_effect(move |previous: Option<Option<ToolCallId>>| {
        let pending = pending_approval();
        if let Some(previous) = previous {
            if let Some(focus) = next_agent_pane_focus(previous.clone(), pending.clone()) {
                agent_pane_focus.set(focus);
            }
            if let Some(next_answered) = next_answered_call(previous, pending.clone()) {
                answered_call.set(next_answered);
            }
        }
        pending
    });
    let turn_in_flight = move || {
        let Some(session_id) = workspace.with(|ws| ws.visible_agent_session_id(index)) else {
            return false;
        };
        frames.with(|frames| frames.agent_frame(session_id).is_turn_in_flight())
    };
    // Drives the pane header's elapsed-time display ("running · 12s") while
    // a turn is in flight. Nothing else in the frame changes while the
    // model is silently thinking/streaming tool arguments, so without this
    // the header would never refresh during exactly the invisible-waiting
    // windows it exists to make legible. `schedule_tick` re-arms itself via
    // `exec_after` — floem's one-shot timer primitive — only as long as
    // `turn_in_flight()` still holds, so it self-terminates the instant the
    // turn ends rather than ticking forever in the background.
    let now_tick = RwSignal::new(std::time::Instant::now());
    fn schedule_tick(
        now_tick: RwSignal<std::time::Instant>,
        turn_in_flight: impl Fn() -> bool + 'static + Copy,
    ) {
        let interval = std::time::Duration::from_secs(crate::agent::pane_status_tick_secs());
        floem::action::exec_after(interval, move |_| {
            now_tick.set(std::time::Instant::now());
            if turn_in_flight() {
                schedule_tick(now_tick, turn_in_flight);
            }
        });
    }
    create_effect(move |was_in_flight: Option<bool>| {
        let in_flight = turn_in_flight();
        if in_flight && was_in_flight != Some(true) {
            schedule_tick(now_tick, turn_in_flight);
        }
        in_flight
    });
    let agent_status_text = move || {
        if !is_agent() {
            return None;
        }
        let state = agent_session_state()?;
        let session_id = workspace.with(|ws| ws.visible_agent_session_id(index))?;
        let entered_at = frames.with(|frames| frames.agent_state_entered_at(session_id))?;
        let elapsed =
            turn_in_flight().then(|| now_tick.get().saturating_duration_since(entered_at));
        Some(agent_pane_status_label(state, elapsed))
    };
    let agent_draft = agent_drafts[index];

    let cancel_visible = move || is_agent() && turn_in_flight();

    v_stack((
        pane_header(
            title,
            agent_status_text,
            active,
            closeable,
            cancel_visible,
            {
                let command_state = command_state.clone();
                move || {
                    cancel_requested.set(true);
                    let Some(session_id) =
                        workspace.with_untracked(|ws| ws.visible_agent_session_id(index))
                    else {
                        return;
                    };
                    execute_command(
                        CommandInvocation::CancelAgentTurn { session_id },
                        command_state.clone(),
                    );
                }
            },
            {
                let command_state = command_state.clone();
                move || {
                    execute_command(
                        CommandInvocation::ClosePane { index },
                        command_state.clone(),
                    );
                }
            },
        ),
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
        agent_approval_banner(
            is_agent,
            pending_approval,
            move || answered_call.get(),
            pending_summary,
            pending_approval_extra,
            {
                let command_state = command_state.clone();
                move |call_id| {
                    answer_pending(answered_call, agent_pane_focus, call_id, |call_id| {
                        approve_pending(workspace, command_state.clone(), index, call_id)
                    });
                }
            },
            {
                let command_state = command_state.clone();
                move |call_id| {
                    answer_pending(answered_call, agent_pane_focus, call_id, |call_id| {
                        deny_pending(workspace, command_state.clone(), index, call_id)
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
        workspace_mode_scrim(workspace_mode_active),
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
            // A click always "dives" into the pane clicked while workspace
            // mode is active -- the design's mouse convention
            // (`docs/workspace-mode-design.md`): commit relative to the
            // click target, exiting the mode, rather than leaving the mode
            // active with only focus moved. A no-op when the mode isn't
            // active, in which case the ordinary `activate_visible_pane`
            // call right below is this click's entire effect, unchanged.
            ws.commit_workspace_mode_to(index);
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

            // Workspace mode's entry chord (default `ctrl+'`,
            // `docs/workspace-mode-design.md`) always wins, regardless of
            // pane kind or any pane-internal focus (e.g. the approval
            // banner below, which would otherwise swallow a held-modifier
            // chord as `BannerKeyAction::Swallow`) -- it's the one
            // irreducible escape hatch back to the mode and must never be
            // capturable by anything in-pane. Re-pressing it while already
            // in the mode is a no-op (`Workspace::enter_workspace_mode`).
            if is_workspace_mode_enter_key(key_event) {
                if !workspace.with_untracked(|ws| ws.is_workspace_mode_active()) {
                    execute_command(CommandInvocation::EnterWorkspaceMode, command_state.clone());
                }
                return EventPropagation::Stop;
            }

            // While workspace mode is active, every key belongs to it --
            // recognized ones (`hjkl`/Enter/Esc/`:`) dispatch below,
            // everything else is silently swallowed rather than reaching
            // the banner/terminal/agent-draft handling further down.
            if workspace.with_untracked(|ws| ws.is_workspace_mode_active()) {
                if let Some(action) =
                    handle_workspace_mode_key(key_event, ime_composing, ime_preedit)
                {
                    match action {
                        ModeAction::Move(direction) => execute_command(
                            CommandInvocation::MoveWorkspaceCursor { direction },
                            command_state.clone(),
                        ),
                        ModeAction::Commit => execute_command(
                            CommandInvocation::CommitWorkspaceMode,
                            command_state.clone(),
                        ),
                        ModeAction::Cancel => execute_command(
                            CommandInvocation::CancelWorkspaceMode,
                            command_state.clone(),
                        ),
                        ModeAction::OpenPalette => {
                            ime_composing.set(false);
                            ime_preedit.set(None);
                            set_ime_allowed(false);
                            control_mode.set(ControlMode::Commands);
                            open_palette(open_palette_state);
                        }
                    }
                }
                return EventPropagation::Stop;
            }
        }

        // While the approval banner holds pane-internal focus, it answers
        // for every key here (even ones bound to nothing -- see
        // `BannerKeyAction::Swallow`) except the one soft-redirect path, so
        // this must run before `handle_active_pane_key` ever sees the key
        // (targeting discipline: only this pane's own session/call_id are
        // ever touched, via `approve_pending`/`deny_pending` above).
        if let Event::KeyDown(key_event) = event {
            if is_agent() && agent_pane_focus.get_untracked() == AgentPaneFocus::Banner {
                match handle_agent_banner_key(key_event, ime_composing, ime_preedit) {
                    BannerKeyAction::Approve => {
                        if let Some(call_id) =
                            awaiting_call(pending_approval(), answered_call.get_untracked())
                        {
                            answer_pending(answered_call, agent_pane_focus, call_id, |call_id| {
                                approve_pending(workspace, command_state.clone(), index, call_id);
                            });
                        }
                    }
                    BannerKeyAction::Deny => {
                        if let Some(call_id) =
                            awaiting_call(pending_approval(), answered_call.get_untracked())
                        {
                            answer_pending(answered_call, agent_pane_focus, call_id, |call_id| {
                                deny_pending(workspace, command_state.clone(), index, call_id);
                            });
                        }
                    }
                    BannerKeyAction::ReleaseFocus => {
                        agent_pane_focus.set(AgentPaneFocus::MessageBox);
                    }
                    BannerKeyAction::Redirect(text) => {
                        agent_pane_focus.set(AgentPaneFocus::MessageBox);
                        agent_draft.update(|draft| draft.push_str(&text));
                    }
                    BannerKeyAction::Swallow => {}
                }
                return EventPropagation::Stop;
            }
        }

        // An agent pane's message box (unlike a terminal) has no
        // protocol-level claim on a bare `Esc`, so it doubles as a second
        // workspace-mode entry path -- but only once the banner above has
        // had first refusal, so the banner's own `Esc`-releases-focus
        // behavior isn't shadowed while it holds pane-internal focus. See
        // `docs/workspace-mode-design.md`'s per-kind asymmetry.
        if let Event::KeyDown(key_event) = event {
            if is_agent()
                && agent_escape_requests_workspace_mode(key_event, ime_composing.get_untracked())
            {
                execute_command(CommandInvocation::EnterWorkspaceMode, command_state.clone());
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
                ime_preedit,
                agent_draft,
            ) {
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    })
    .on_event(EventListener::KeyUp, move |event| {
        if let Event::KeyUp(key_event) = event {
            if handle_active_pane_key_release(
                key_event,
                workspace,
                sessions,
                index,
                ime_composing,
                ime_preedit,
                palette_open,
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

        // Workspace mode's cursor frame (`docs/workspace-mode-design.md`):
        // visually distinct from the focus border below in both color
        // (`theme::cursor_accent()`, a role dedicated to this) and
        // thickness, so the two remain simultaneously legible when the
        // cursor has moved away from focus.
        let (border_width, border) = if is_cursor() {
            (WORKSPACE_MODE_CURSOR_BORDER_WIDTH, theme::cursor_accent())
        } else if active() {
            (1.0, theme::accent())
        } else {
            (1.0, theme::surface_selected())
        };
        s.height_full()
            .min_width(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .background(theme::surface_panel())
            .border(border_width)
            .border_color(border)
    })
}

/// The cursor frame's border width -- thicker than the ordinary focus/
/// unselected border (`1.0`, above) so the cursor pane is unmistakable at a
/// glance even before reading its color.
const WORKSPACE_MODE_CURSOR_BORDER_WIDTH: f64 = 2.0;
