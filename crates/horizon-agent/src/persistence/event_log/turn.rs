use uuid::Uuid;

use crate::contract::{Event, MessageRole, SessionState};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TurnTracker {
    current_turn_id: Option<String>,
}

impl TurnTracker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn turn_id_for_event(&mut self, event: &Event) -> Option<String> {
        if matches!(
            event,
            Event::MessageCommitted(message) if message.role == MessageRole::User
        ) {
            self.current_turn_id = Some(Uuid::new_v4().to_string());
        }

        let turn_id = self.current_turn_id.clone();

        if matches!(
            event,
            Event::StateChanged(
                SessionState::WaitingForUser
                    | SessionState::WaitingForApproval
                    | SessionState::Cancelled
                    | SessionState::Failed
                    | SessionState::Terminated
            )
        ) {
            self.current_turn_id = None;
        }

        turn_id
    }
}
