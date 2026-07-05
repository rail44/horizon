use std::time::Duration;

use crate::agent::contract::{SessionState, ToolCallId};
use crate::ui::fonts::font_family;
use crate::ui::hint_chip::key_hint;
use crate::ui::theme;
use floem::prelude::*;
use floem::{
    action::set_ime_cursor_area,
    peniko::kurbo::{Point, Size},
};

pub(super) fn agent_composer(
    visible: impl Fn() -> bool + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
    draft: RwSignal<String>,
    preedit: impl Fn() -> Option<String> + 'static + Copy,
    ime_cursor_area: RwSignal<(Point, Size)>,
) -> impl IntoView {
    label(move || {
        let text = draft.get();
        let preedit = preedit().unwrap_or_default();
        if text.is_empty() && preedit.is_empty() {
            "Message agent...".to_string()
        } else if preedit.is_empty() {
            text
        } else {
            format!("{text}{preedit}")
        }
    })
    .style(move |s| {
        if !visible() {
            return s.hide();
        }

        let border = if active() {
            floem::peniko::Color::from_rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::from_rgb8(57, 64, 76)
        };
        let color = if draft.with(|text| text.is_empty()) && preedit().is_none() {
            floem::peniko::Color::from_rgb8(115, 122, 136)
        } else {
            floem::peniko::Color::from_rgb8(233, 236, 242)
        };

        s.width_full()
            .height(34)
            .min_height(34)
            .items_center()
            .padding_horiz(10)
            .margin_horiz(8)
            .margin_bottom(7)
            .font_family(font_family().to_string())
            .font_size(12)
            .line_height(1.2)
            .color(color)
            .background(floem::peniko::Color::from_rgb8(21, 24, 30))
            .border(1.0)
            .border_color(border)
    })
    .on_move(move |origin| {
        if active() && visible() {
            let position = origin + Point::new(10.0, 6.0).to_vec2();
            let size = Size::new(8.0, 18.0);
            ime_cursor_area.set((position, size));
            set_ime_cursor_area(position, size);
        }
    })
}

/// Which of an agent pane's two pane-internal focus targets currently owns
/// the keyboard: the message-box composer, or the approval banner (see
/// `agent_approval_banner` below). Only meaningful for agent panes --
/// terminal panes have no banner and their pane never leaves `MessageBox`.
///
/// Design: the tool-approval interaction takes its "every interaction
/// request visibly explains which key does what" cue from crush
/// (charmbracelet's TUI) -- see `ui::hint_chip::key_hint` for the inline
/// `[y] approve` bindings this drives, and `workspace::input::
/// handle_agent_banner_key` for the key routing this focus state gates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AgentPaneFocus {
    #[default]
    MessageBox,
    Banner,
}

/// Computes the pane's next approval-banner focus, if it should change,
/// after the oldest pending tool-call approval (`AgentFrame::
/// pending_approval_call_id`, gated by `gate_pending_approval`) goes from
/// `previous_pending` to `pending`.
///
/// Returns `None` when the pending call id didn't actually change -- an
/// unrelated frame update, or the same call still pending after the user
/// pressed `Esc` to leave the banner early (a no-op refresh must not undo
/// that). Otherwise: a *new* call becoming the oldest pending one (the
/// session's first pending approval, or the next one in a queue once the
/// previous oldest resolved) grabs the banner; no call pending any more
/// releases it back to the message box. This covers both halves of the
/// design -- the banner "takes... focus" the instant a request arrives, and
/// "focus returns to the message box" the instant it resolves -- regardless
/// of how the resolution happened, since a key, a click, and a palette
/// action all converge on the same `pending_approval` signal.
pub(super) fn next_agent_pane_focus(
    previous_pending: Option<ToolCallId>,
    pending: Option<ToolCallId>,
) -> Option<AgentPaneFocus> {
    if previous_pending == pending {
        return None;
    }
    Some(if pending.is_some() {
        AgentPaneFocus::Banner
    } else {
        AgentPaneFocus::MessageBox
    })
}

/// The approval banner: renders only the oldest pending tool call's
/// approve/deny controls (targeting discipline -- banner keys and buttons
/// alike act on the oldest pending call only; cross-session approvals stay
/// palette-only) with their key bindings spelled out inline
/// (`ui::hint_chip::key_hint`). `extra_pending` is the count of additional
/// calls queued behind the one shown, rendered as a "+N more" hint so a
/// second or third pending approval isn't silently dropped from view.
pub(super) fn agent_approval_banner(
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    extra_pending: impl Fn() -> usize + 'static + Copy,
    on_approve: impl Fn(ToolCallId) + 'static,
    on_deny: impl Fn(ToolCallId) + 'static,
) -> impl IntoView {
    h_stack((
        agent_approval_button(
            "y",
            "approve",
            visible,
            pending_approval,
            on_approve,
            floem::peniko::Color::from_rgb8(48, 84, 75),
            theme::accent(),
        ),
        agent_approval_button(
            "n",
            "deny",
            visible,
            pending_approval,
            on_deny,
            floem::peniko::Color::from_rgb8(80, 50, 54),
            theme::danger(),
        ),
        key_hint("esc", "back to draft"),
        label(move || format!("+{} more", extra_pending())).style(move |s| {
            if extra_pending() == 0 {
                return s.hide();
            }

            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_subtle())
        }),
    ))
    .style(move |s| {
        if !visible() || pending_approval().is_none() {
            return s.hide();
        }

        s.width_full()
            .height(30)
            .min_height(30)
            .items_center()
            .justify_end()
            .padding_horiz(8)
            .gap(10)
    })
}

/// Compact pane-header label for an agent session's current state, e.g.
/// `"considering · 12s"` while a turn is in flight or plain `"cancelled"`
/// once it's settled. `elapsed` is `Some` only while ticking makes sense
/// (see the pane's `turn_in_flight`-gated timer) — otherwise the state name
/// is shown on its own. Satisfies `docs/ux-principles.md`'s Persistent UI
/// Requirement that the pane header show pane state.
pub(super) fn agent_pane_status_label(state: SessionState, elapsed: Option<Duration>) -> String {
    let label = agent_state_label(state);
    match elapsed {
        Some(elapsed) => format!("{label} · {}s", elapsed.as_secs()),
        None => label.to_string(),
    }
}

fn agent_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Created => "starting",
        SessionState::Running => "considering",
        SessionState::WaitingForUser => "idle",
        SessionState::WaitingForApproval => "waiting for approval",
        SessionState::ToolRunning => "running tool",
        SessionState::Cancelled => "cancelled",
        SessionState::Completed => "done",
        SessionState::Failed => "failed",
        SessionState::Terminated => "terminated",
    }
}

/// Gates the pane's pending approval behind its "cancel requested" latch.
///
/// Once the user clicks Cancel, approve/deny must go dead immediately — not
/// after the provider's cancel events round-trip. In that window the call
/// still looks pending in the frame, and an Approve click would really
/// execute the tool while history records the call as cancelled. The latch
/// is set the instant the cancel action fires and cleared when the frame's
/// session state next changes (by then the cancelled call has resolved, or
/// the cancel was ignored and the approval is genuinely still pending).
pub(super) fn gate_pending_approval(
    cancel_requested: bool,
    pending: Option<ToolCallId>,
) -> Option<ToolCallId> {
    if cancel_requested {
        None
    } else {
        pending
    }
}

fn agent_approval_button(
    key_label: &'static str,
    action_label: &'static str,
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_click: impl Fn(ToolCallId) + 'static,
    background: floem::peniko::Color,
    border: floem::peniko::Color,
) -> impl IntoView {
    key_hint(key_label, action_label)
        .on_click_stop(move |_| {
            if let Some(call_id) = pending_approval() {
                on_click(call_id);
            }
        })
        .style(move |s| {
            if !visible() || pending_approval().is_none() {
                return s.hide();
            }

            s.height(26)
                .padding_horiz(10)
                .items_center()
                .justify_center()
                .background(background)
                .border(1.0)
                .border_color(border)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        agent_pane_status_label, gate_pending_approval, next_agent_pane_focus, AgentPaneFocus,
    };
    use crate::agent::contract::{SessionState, ToolCallId};
    use std::time::Duration;

    #[test]
    fn agent_pane_status_label_appends_elapsed_when_present() {
        assert_eq!(
            agent_pane_status_label(SessionState::Running, Some(Duration::from_secs(12))),
            "considering · 12s"
        );
    }

    #[test]
    fn agent_pane_status_label_omits_elapsed_when_absent() {
        assert_eq!(
            agent_pane_status_label(SessionState::Cancelled, None),
            "cancelled"
        );
    }

    #[test]
    fn cancel_request_gates_pending_approval() {
        let pending = Some(ToolCallId("call-1".to_string()));
        assert_eq!(gate_pending_approval(true, pending), None);
        assert_eq!(gate_pending_approval(true, None), None);
    }

    #[test]
    fn pending_approval_passes_through_without_cancel_request() {
        let pending = Some(ToolCallId("call-1".to_string()));
        assert_eq!(gate_pending_approval(false, pending.clone()), pending);
        assert_eq!(gate_pending_approval(false, None), None);
    }

    // --- approval banner focus transitions (`next_agent_pane_focus`) -----

    #[test]
    fn banner_auto_focuses_when_a_call_first_becomes_pending() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(None, call),
            Some(AgentPaneFocus::Banner)
        );
    }

    #[test]
    fn banner_releases_focus_once_the_call_resolves() {
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(
            next_agent_pane_focus(call, None),
            Some(AgentPaneFocus::MessageBox)
        );
    }

    #[test]
    fn banner_stays_focused_when_the_oldest_pending_call_changes() {
        // The previous oldest resolved, revealing the next one in the queue
        // -- the banner should keep focus rather than release it.
        let first = Some(ToolCallId("call-1".to_string()));
        let second = Some(ToolCallId("call-2".to_string()));
        assert_eq!(
            next_agent_pane_focus(first, second),
            Some(AgentPaneFocus::Banner)
        );
    }

    #[test]
    fn banner_focus_is_unchanged_when_the_pending_call_id_is_the_same() {
        // Must be a true no-op: this is what lets `Esc` (which doesn't
        // change the pending call, only the focus state) survive an
        // unrelated frame refresh without being fought back to `Banner`.
        let call = Some(ToolCallId("call-1".to_string()));
        assert_eq!(next_agent_pane_focus(call.clone(), call), None);
        assert_eq!(next_agent_pane_focus(None, None), None);
    }
}
