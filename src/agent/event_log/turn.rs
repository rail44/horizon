use uuid::Uuid;

use crate::agent::{AgentEvent, AgentMessageRole, AgentSessionState};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentTurnTracker {
    current_turn_id: Option<String>,
}

impl AgentTurnTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn turn_id_for_event(&mut self, event: &AgentEvent) -> Option<String> {
        if matches!(
            event,
            AgentEvent::MessageCommitted(message) if message.role == AgentMessageRole::User
        ) {
            self.current_turn_id = Some(Uuid::new_v4().to_string());
        }

        let turn_id = self.current_turn_id.clone();

        if matches!(
            event,
            AgentEvent::StateChanged(
                AgentSessionState::WaitingForUser
                    | AgentSessionState::WaitingForApproval
                    | AgentSessionState::Failed
                    | AgentSessionState::Terminated
            )
        ) {
            self.current_turn_id = None;
        }

        turn_id
    }
}
