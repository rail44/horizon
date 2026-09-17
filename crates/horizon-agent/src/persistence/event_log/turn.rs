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

    pub(super) fn has_open_turn(&self) -> bool {
        self.current_turn_id.is_some()
    }

    pub(super) fn restore(&mut self, event: &Event, turn_id: Option<&str>) {
        if ends_turn(event) {
            self.current_turn_id = None;
        } else if let Some(turn_id) = turn_id {
            self.current_turn_id = Some(turn_id.to_string());
        }
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
                Event::MessageCommitted(message)
                    if message.role == MessageRole::TaskNotification
                        || message.role == MessageRole::AutoContinue
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
        if ends_turn(event) {
            self.current_turn_id = None;
        }

        turn_id
    }
}

fn ends_turn(event: &Event) -> bool {
    matches!(event, Event::TurnEnded(_))
        || matches!(
            event,
            Event::StateChanged(
                SessionState::WaitingForUser
                    | SessionState::Cancelled
                    | SessionState::Failed
                    | SessionState::Terminated
            )
        )
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

    #[test]
    fn recovery_preserves_the_original_turn_and_starts_a_new_identity_after_it() {
        let mut original = TurnTracker::new();
        let id = original
            .turn_id_for_event(&committed(MessageRole::User))
            .unwrap();
        let mut restored = TurnTracker::new();
        restored.restore(&committed(MessageRole::User), Some(&id));
        restored.restore(
            &Event::StateChanged(SessionState::WaitingForApproval),
            Some(&id),
        );
        restored.restore(&Event::DeliveryAcknowledged("other-input".into()), None);
        assert_eq!(
            restored.turn_id_for_event(&Event::TurnEnded(TurnEndReason::Cancelled)),
            Some(id.clone())
        );
        assert_eq!(
            restored.turn_id_for_event(&Event::StateChanged(SessionState::WaitingForUser)),
            None
        );
        let next = restored
            .turn_id_for_event(&committed(MessageRole::User))
            .unwrap();
        assert_ne!(next, id);
    }

    #[test]
    fn recovery_does_not_reopen_a_turn_past_any_persisted_boundary() {
        for boundary in [
            Event::TurnEnded(TurnEndReason::Completed),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Cancelled),
            Event::StateChanged(SessionState::Failed),
            Event::StateChanged(SessionState::Terminated),
        ] {
            let mut restored = TurnTracker::new();
            restored.restore(&committed(MessageRole::User), Some("old-turn"));
            restored.restore(&boundary, Some("old-turn"));
            restored.restore(&Event::DeliveryAcknowledged("input".into()), None);
            assert_eq!(
                restored.turn_id_for_event(&Event::StateChanged(SessionState::Created)),
                None
            );
        }
    }
}
