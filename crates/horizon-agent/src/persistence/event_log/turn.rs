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

        // A turn ends at the provider's explicit boundary (`TurnEnded`) or
        // when the session reaches a terminal state. `WaitingForUser` is the
        // post-turn idle state and is therefore also a boundary marker, but
        // `WaitingForApproval` is mid-turn: the user is still inside the same
        // turn while deciding on a tool call.
        if matches!(event, Event::TurnEnded(_))
            || matches!(
                event,
                Event::StateChanged(
                    SessionState::WaitingForUser
                        | SessionState::Cancelled
                        | SessionState::Failed
                        | SessionState::Terminated
                )
            )
        {
            self.current_turn_id = None;
        }

        turn_id
    }
}
