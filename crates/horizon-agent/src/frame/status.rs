//! Current session status combines execution phase with its stop result.

use super::AgentFrame;
use crate::contract::{SessionState, TurnEndReason};

/// A read-only status derived from existing events, shared by session clients.
/// WaitingForInput includes normal answer completion; it does not assert that
/// the owner must respond. Failed describes the stopped turn, not a dead session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Running,
    ToolRunning,
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Paused,
    Completed,
    Failed,
    Terminated,
}

impl AgentFrame {
    /// The current phase and stop result, without scanning transcript history.
    /// Failed/cancelled/paused results survive an idle state and replay until
    /// a new execution starts. Tool failures alone do not end the turn.
    pub fn status(&self) -> Option<SessionStatus> {
        Some(match self.state? {
            SessionState::Created => SessionStatus::Starting,
            SessionState::Running => SessionStatus::Running,
            SessionState::ToolRunning => SessionStatus::ToolRunning,
            SessionState::WaitingForApproval => SessionStatus::WaitingForApproval,
            SessionState::WaitingForUser => match self.turn_end_reason {
                Some(TurnEndReason::Failed) => SessionStatus::Failed,
                Some(TurnEndReason::Cancelled) => SessionStatus::Cancelled,
                Some(
                    TurnEndReason::Halted
                    | TurnEndReason::HaltedByIterationCap
                    | TurnEndReason::HaltedByDoomLoop,
                ) => SessionStatus::Paused,
                Some(TurnEndReason::Completed) | None => SessionStatus::WaitingForInput,
            },
            SessionState::Cancelled => SessionStatus::Cancelled,
            SessionState::Completed => SessionStatus::Completed,
            SessionState::Failed => SessionStatus::Failed,
            SessionState::Terminated => SessionStatus::Terminated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Error, Event, Message, MessageRole};
    use crate::frame::{agent_frame_from_events, apply_agent_event_to_frame, TurnClock};

    #[test]
    fn failed_turn_survives_idle_and_restart_until_work_actually_resumes() {
        let mut frame = AgentFrame::empty();
        let mut clock = TurnClock::new();
        let mut history = Vec::new();
        let steps = [
            (
                Event::StateChanged(SessionState::Running),
                SessionStatus::Running,
            ),
            (
                Event::Error(Error {
                    message: "Provider timed out".into(),
                }),
                SessionStatus::Running,
            ),
            (
                Event::TurnEnded(TurnEndReason::Failed),
                SessionStatus::Running,
            ),
            (
                Event::StateChanged(SessionState::WaitingForUser),
                SessionStatus::Failed,
            ),
            (Event::SessionResumed, SessionStatus::Failed),
            (
                Event::StateChanged(SessionState::Created),
                SessionStatus::Starting,
            ),
            (
                Event::MessageCommitted(Message {
                    role: MessageRole::Assistant,
                    text: "Session restored".into(),
                }),
                SessionStatus::Starting,
            ),
            (
                Event::StateChanged(SessionState::WaitingForUser),
                SessionStatus::Failed,
            ),
            (
                Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "Try again".into(),
                }),
                SessionStatus::Failed,
            ),
            (
                Event::StateChanged(SessionState::Running),
                SessionStatus::Running,
            ),
            (
                Event::StateChanged(SessionState::WaitingForApproval),
                SessionStatus::WaitingForApproval,
            ),
            (
                Event::StateChanged(SessionState::ToolRunning),
                SessionStatus::ToolRunning,
            ),
            (
                Event::TurnEnded(TurnEndReason::Completed),
                SessionStatus::ToolRunning,
            ),
            (
                Event::StateChanged(SessionState::WaitingForUser),
                SessionStatus::WaitingForInput,
            ),
        ];
        for (event, expected) in steps {
            apply_agent_event_to_frame(&mut frame, &event, &mut clock);
            history.push(event);
            assert_eq!(frame.status(), Some(expected));
            assert_eq!(agent_frame_from_events(&history).status(), frame.status());
        }
    }

    #[test]
    fn recovered_errors_and_user_cancellation_are_not_failed_turns() {
        for (reason, expected) in [
            (TurnEndReason::Completed, SessionStatus::WaitingForInput),
            (TurnEndReason::Cancelled, SessionStatus::Cancelled),
            (TurnEndReason::HaltedByIterationCap, SessionStatus::Paused),
            (TurnEndReason::HaltedByDoomLoop, SessionStatus::Paused),
        ] {
            let events = [
                Event::StateChanged(SessionState::Running),
                Event::Error(Error {
                    message: "An error that was handled".into(),
                }),
                Event::TurnEnded(reason),
                Event::StateChanged(SessionState::WaitingForUser),
            ];
            assert_eq!(agent_frame_from_events(&events).status(), Some(expected));
        }
    }

    #[test]
    fn automatic_recovery_clears_the_failed_result_on_start() {
        let frame = agent_frame_from_events(&[
            Event::TurnEnded(TurnEndReason::Failed),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Running),
        ]);
        assert_eq!(frame.status(), Some(SessionStatus::Running));
        assert_eq!(frame.turn_end_reason, None);
        assert_eq!(frame.last_turn_end_reason(), Some(TurnEndReason::Failed));
    }

    #[test]
    fn explicit_failure_state_also_survives_provider_initialization() {
        let frame = agent_frame_from_events(&[
            Event::StateChanged(SessionState::Failed),
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
        ]);
        assert_eq!(frame.status(), Some(SessionStatus::Failed));
    }
}
