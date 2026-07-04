use std::time::Duration;

use crate::agent::contract::{SessionState, ToolCallId};
use crate::ui::fonts::HORIZON_FONT_FAMILY;
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
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(57, 64, 76)
        };
        let color = if draft.with(|text| text.is_empty()) && preedit().is_none() {
            floem::peniko::Color::rgb8(115, 122, 136)
        } else {
            floem::peniko::Color::rgb8(233, 236, 242)
        };

        s.width_full()
            .height(34)
            .min_height(34)
            .items_center()
            .padding_horiz(10)
            .margin_horiz(8)
            .margin_bottom(7)
            .font_family(HORIZON_FONT_FAMILY.to_string())
            .font_size(12)
            .line_height(1.2)
            .color(color)
            .background(floem::peniko::Color::rgb8(21, 24, 30))
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

pub(super) fn agent_approval_actions(
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_approve: impl Fn(ToolCallId) + 'static,
    on_deny: impl Fn(ToolCallId) + 'static,
) -> impl IntoView {
    h_stack((
        agent_approval_button(
            "Approve",
            visible,
            pending_approval,
            on_approve,
            floem::peniko::Color::rgb8(48, 84, 75),
            floem::peniko::Color::rgb8(132, 220, 198),
        ),
        agent_approval_button(
            "Deny",
            visible,
            pending_approval,
            on_deny,
            floem::peniko::Color::rgb8(80, 50, 54),
            floem::peniko::Color::rgb8(246, 137, 146),
        ),
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
            .gap(8)
    })
}

pub(super) fn agent_cancel_action(
    visible: impl Fn() -> bool + 'static + Copy,
    turn_in_flight: impl Fn() -> bool + 'static + Copy,
    on_cancel: impl Fn() + 'static,
) -> impl IntoView {
    label(|| "Cancel turn".to_string())
        .on_click_stop(move |_| {
            if turn_in_flight() {
                on_cancel();
            }
        })
        .style(move |s| {
            if !visible() || !turn_in_flight() {
                return s.hide();
            }

            s.width_full()
                .height(30)
                .min_height(30)
                .items_center()
                .justify_end()
                .padding_horiz(20)
                .font_family(HORIZON_FONT_FAMILY.to_string())
                .font_size(12)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
                .background(floem::peniko::Color::rgb8(74, 60, 40))
                .border(1.0)
                .border_color(floem::peniko::Color::rgb8(224, 176, 108))
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
    text: &'static str,
    visible: impl Fn() -> bool + 'static + Copy,
    pending_approval: impl Fn() -> Option<ToolCallId> + 'static + Copy,
    on_click: impl Fn(ToolCallId) + 'static,
    background: floem::peniko::Color,
    border: floem::peniko::Color,
) -> impl IntoView {
    label(move || text.to_string())
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
                .padding_horiz(12)
                .items_center()
                .justify_center()
                .font_family(HORIZON_FONT_FAMILY.to_string())
                .font_size(12)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
                .background(background)
                .border(1.0)
                .border_color(border)
        })
}

#[cfg(test)]
mod tests {
    use super::{agent_pane_status_label, gate_pending_approval};
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
}
