//! Session-status projection and its small, ordinary entity view.

use gpui::*;
use horizon_agent::contract::SessionState;
use horizon_agent::frame::state_indicates_turn_in_flight;

use super::super::session::AgentSession;
use super::transcript::render_stop_button;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusTone {
    Muted,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusProjection {
    text: &'static str,
    tone: StatusTone,
    turn_in_flight: bool,
}

fn project_status(state: Option<SessionState>, runtime_unreachable: bool) -> StatusProjection {
    // A dead sessiond channel wins over the folded session state: all pane
    // interactions are otherwise heading nowhere. The independent in-flight
    // bit keeps Stop reachable even while that error is shown.
    if runtime_unreachable {
        return StatusProjection {
            text: "session runtime unreachable — try Reload Session Runtime",
            tone: StatusTone::Danger,
            turn_in_flight: state_indicates_turn_in_flight(state),
        };
    }

    let text = match state {
        Some(SessionState::Running) => "running…",
        Some(SessionState::ToolRunning) => "tool running…",
        Some(SessionState::WaitingForApproval) => "waiting for approval",
        Some(SessionState::WaitingForUser) | Some(SessionState::Created) | None => "",
        Some(SessionState::Cancelled) => "cancelled",
        Some(SessionState::Completed) => "completed",
        Some(SessionState::Failed) => "failed",
        Some(SessionState::Terminated) => "terminated",
        Some(SessionState::Unknown) => "",
    };
    StatusProjection {
        text,
        tone: StatusTone::Muted,
        turn_in_flight: state_indicates_turn_in_flight(state),
    }
}

pub(super) struct AgentStatus {
    projection: StatusProjection,
    _session_subscription: Subscription,
}

impl AgentStatus {
    pub(super) fn new(session: Entity<AgentSession>, cx: &mut Context<Self>) -> Self {
        let projection = Self::project(&session, cx);
        let subscription = cx.observe(&session, |status: &mut Self, session, cx| {
            let session = session.read(cx);
            let next = project_status(session.frame.state, session.runtime_unreachable());
            if status.projection != next {
                status.projection = next;
                cx.notify();
            }
        });
        Self {
            projection,
            _session_subscription: subscription,
        }
    }

    fn project(session: &Entity<AgentSession>, cx: &App) -> StatusProjection {
        let session = session.read(cx);
        project_status(session.frame.state, session.runtime_unreachable())
    }
}

impl Render for AgentStatus {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.projection.text.is_empty() && !self.projection.turn_in_flight {
            return Empty.into_any_element();
        }
        let color = match self.projection.tone {
            StatusTone::Muted => theme::text_muted(),
            StatusTone::Danger => theme::danger(),
        };
        let mut row = div()
            .px_2()
            .py_0p5()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(color)
                    .child(self.projection.text),
            );
        if self.projection.turn_in_flight {
            row = row.child(render_stop_button("status-line-stop"));
        }
        row.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::SessionState;

    use super::{project_status, StatusTone};

    #[test]
    fn runtime_failure_wins_and_turn_state_controls_the_stop_affordance() {
        let projection = project_status(Some(SessionState::Running), true);
        assert_eq!(projection.tone, StatusTone::Danger);
        assert_eq!(
            projection.text,
            "session runtime unreachable — try Reload Session Runtime"
        );
        assert!(projection.turn_in_flight);

        for state in [
            None,
            Some(SessionState::Created),
            Some(SessionState::Unknown),
        ] {
            let projection = project_status(state, false);
            assert_eq!(projection.text, "");
            assert!(!projection.turn_in_flight);
            assert_eq!(projection.tone, StatusTone::Muted);
        }

        let projection = project_status(Some(SessionState::ToolRunning), false);
        assert_eq!(projection.text, "tool running…");
        assert!(projection.turn_in_flight);
    }
}
