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
        // A background-`task` notification (`MessageRole::TaskNotification`)
        // opens a turn only when none is open. It is delivered in two
        // shapes and they need opposite treatment: injected into an
        // already-running turn's next provider round (must stay inside that
        // turn, or the turn's own `TurnEnded` would land under a second
        // turn id), or as the synthetic input of an auto-started turn after
        // the previous one ended (must open one, or every event of that
        // turn would be recorded with no turn id at all). "Is a turn
        // currently open" is exactly the discriminator, and this tracker
        // already holds it.
        if self.current_turn_id.is_none()
            && matches!(
                event,
                Event::MessageCommitted(message) if message.role == MessageRole::TaskNotification
            )
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Message, TurnEndReason};

    fn committed(role: MessageRole) -> Event {
        Event::MessageCommitted(Message {
            role,
            text: "x".to_string(),
        })
    }

    /// The background-`task` notification's two delivery shapes need
    /// opposite treatment, and "is a turn currently open" is the whole
    /// discriminator: injected mid-turn it must stay inside the running
    /// turn (or that turn's own `TurnEnded` would be attributed to a second
    /// turn id), while an auto-started turn's notification must open one
    /// (or every event of that turn would be recorded with no turn id).
    #[test]
    fn a_task_notification_opens_a_turn_only_when_none_is_open() {
        let mut tracker = TurnTracker::new();

        let user_turn = tracker
            .turn_id_for_event(&committed(MessageRole::User))
            .expect("a user message opens a turn");
        assert_eq!(
            tracker.turn_id_for_event(&committed(MessageRole::TaskNotification)),
            Some(user_turn.clone()),
            "a mid-turn notification must not split the turn it was injected into"
        );
        assert_eq!(
            tracker.turn_id_for_event(&Event::TurnEnded(TurnEndReason::Completed)),
            Some(user_turn.clone())
        );

        let auto_turn = tracker
            .turn_id_for_event(&committed(MessageRole::TaskNotification))
            .expect("a notification with no turn open must start one");
        assert_ne!(auto_turn, user_turn);
        assert_eq!(
            tracker.turn_id_for_event(&committed(MessageRole::Assistant)),
            Some(auto_turn),
            "the auto-started turn's own events belong to it"
        );
    }
}
