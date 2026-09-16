//! Session-status projection and its small, ordinary entity view.

use gpui::*;
use gpui_component::status_bar::StatusBar;
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

/// The display label for one folded `SessionState`, shared by the status
/// line and the transcript's running card so the two wordings can't
/// drift. `None` for the quiet states (`Created`/`WaitingForUser`) the
/// status line hides entirely; each caller picks its own fallback.
pub(super) fn session_state_label(state: SessionState) -> Option<&'static str> {
    match state {
        SessionState::Running => Some("running…"),
        SessionState::ToolRunning => Some("tool running…"),
        SessionState::WaitingForApproval => Some("waiting for approval"),
        SessionState::Cancelled => Some("cancelled"),
        SessionState::Completed => Some("completed"),
        SessionState::Failed => Some("failed"),
        SessionState::Terminated => Some("terminated"),
        SessionState::Created | SessionState::WaitingForUser => None,
    }
}

fn project_status(state: Option<SessionState>, runtime_unreachable: bool) -> StatusProjection {
    // A dead agentd channel wins over the folded session state: all pane
    // interactions are otherwise heading nowhere. The independent in-flight
    // bit keeps Stop reachable even while that error is shown.
    if runtime_unreachable {
        return StatusProjection {
            text: "session runtime unreachable — try Reload Agent Runtime",
            tone: StatusTone::Danger,
            turn_in_flight: state_indicates_turn_in_flight(state),
        };
    }

    let text = state.and_then(session_state_label).unwrap_or("");
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
        // The component's three-slot status bar chrome (left/center/right)
        // replaces the hand-rolled flex row; the projection above stays ours.
        let mut bar = StatusBar::new().left(
            div()
                .text_size(px(11.0))
                .text_color(color)
                .child(self.projection.text),
        );
        if self.projection.turn_in_flight {
            bar = bar.right(render_stop_button("status-line-stop"));
        }
        bar.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::SessionState;

    use super::{project_status, StatusTone};

    #[test]
    fn session_state_label_covers_every_state_and_hides_the_quiet_ones() {
        use super::session_state_label;

        assert_eq!(session_state_label(SessionState::Running), Some("running…"));
        assert_eq!(
            session_state_label(SessionState::ToolRunning),
            Some("tool running…")
        );
        assert_eq!(
            session_state_label(SessionState::WaitingForApproval),
            Some("waiting for approval")
        );
        assert_eq!(
            session_state_label(SessionState::Cancelled),
            Some("cancelled")
        );
        assert_eq!(
            session_state_label(SessionState::Completed),
            Some("completed")
        );
        assert_eq!(session_state_label(SessionState::Failed), Some("failed"));
        assert_eq!(
            session_state_label(SessionState::Terminated),
            Some("terminated")
        );
        // The quiet states the status line hides entirely.
        assert_eq!(session_state_label(SessionState::Created), None);
        assert_eq!(session_state_label(SessionState::WaitingForUser), None);
    }

    #[test]
    fn runtime_failure_wins_and_turn_state_controls_the_stop_affordance() {
        let projection = project_status(Some(SessionState::Running), true);
        assert_eq!(projection.tone, StatusTone::Danger);
        assert_eq!(
            projection.text,
            "session runtime unreachable — try Reload Agent Runtime"
        );
        assert!(projection.turn_in_flight);

        for state in [None, Some(SessionState::Created)] {
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
