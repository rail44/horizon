//! Where a conversation can branch: the positions between turns, with no
//! input waiting or running (`docs/agent-core-extension-design.md`,
//! "分岐できる位置"). A branch from such a position carries no tool call
//! without its result and no input accepted but not yet handled.

use std::collections::HashSet;

use crate::contract::{Event, MessageRole};

use super::turn::ends_turn;

/// A fold over a branch's events that tells whether the position after the
/// last one is settled.
#[derive(Clone, Debug, Default)]
pub struct Settled {
    queued: HashSet<String>,
    active: Vec<String>,
    turn_open: bool,
}

impl Settled {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_settled(&self) -> bool {
        self.queued.is_empty() && self.active.is_empty() && !self.turn_open
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::InputAccepted(input) => {
                self.queued.insert(input.id.clone());
            }
            Event::InputStarted(ids) => {
                for id in ids {
                    self.queued.remove(id);
                }
                self.active = ids.clone();
            }
            Event::InputOutcome(outcome) => {
                self.queued.retain(|id| !outcome.input_ids.contains(id));
                self.active.retain(|id| !outcome.input_ids.contains(id));
            }
            Event::MessageCommitted(message)
                if matches!(
                    message.role,
                    MessageRole::User | MessageRole::TaskNotification | MessageRole::AutoContinue
                ) =>
            {
                self.turn_open = true;
            }
            event if ends_turn(event) => self.turn_open = false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        InputResult, Message, SessionInput, SessionInputOutcome, SessionState, TurnEndReason,
    };

    fn input(id: &str) -> SessionInput {
        serde_json::from_value(serde_json::json!({
            "id": id, "origin": "user", "text": "hi"
        }))
        .unwrap()
    }

    fn after(events: &[Event]) -> Vec<bool> {
        let mut settled = Settled::new();
        events
            .iter()
            .map(|event| {
                settled.apply(event);
                settled.is_settled()
            })
            .collect()
    }

    #[test]
    fn a_turn_is_settled_only_once_its_input_has_an_outcome() {
        let events = [
            Event::InputAccepted(input("a")),
            Event::InputStarted(vec!["a".into()]),
            Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "hi".into(),
            }),
            Event::TurnEnded(TurnEndReason::Completed),
            Event::InputOutcome(SessionInputOutcome {
                delivery_id: "d".into(),
                input_ids: vec!["a".into()],
                reply_to: None,
                outcome: InputResult::Interrupted,
            }),
            Event::StateChanged(SessionState::WaitingForUser),
        ];
        assert_eq!(after(&events), [false, false, false, false, true, true]);
    }

    #[test]
    fn an_input_queued_during_a_turn_keeps_its_end_unsettled() {
        let events = [
            Event::InputAccepted(input("a")),
            Event::InputStarted(vec!["a".into()]),
            Event::InputAccepted(input("b")),
            Event::InputOutcome(SessionInputOutcome {
                delivery_id: "d".into(),
                input_ids: vec!["a".into()],
                reply_to: None,
                outcome: InputResult::Interrupted,
            }),
        ];
        assert_eq!(after(&events), [false, false, false, false]);
    }
}
