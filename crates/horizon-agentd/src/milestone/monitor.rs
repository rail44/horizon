use horizon_agent::contract::{Event, MessageRole, SessionId, SessionState};

use crate::session::{AgentdState, SessionSubscription};

pub(super) struct Monitor {
    pub(super) item: u64,
    pub(super) token: String,
    pub(super) session: SessionId,
    pub(super) done: bool,
    pub(super) cancelled: bool,
    pub(super) attention: Option<String>,
    pub(super) failure: Option<String>,
    started: bool,
    subscription: SessionSubscription,
}

impl Monitor {
    pub(super) fn new(item: u64, token: String, subscription: SessionSubscription) -> Self {
        Self {
            item,
            token,
            session: subscription.session_id,
            done: false,
            cancelled: false,
            attention: None,
            failure: None,
            started: false,
            subscription,
        }
    }

    pub(super) fn poll(&mut self, state: &AgentdState) {
        while let Ok(event) = self.subscription.events.try_recv() {
            self.observe(event);
        }
        if !state.session_exists(self.session) && !self.done {
            self.failure
                .get_or_insert("The implementation session exited before finishing".into());
            self.done = true;
        }
    }

    fn observe(&mut self, event: Event) {
        match event {
            Event::MessageCommitted(message) if message.role == MessageRole::User => {
                self.started = true
            }
            Event::StateChanged(SessionState::WaitingForUser) if self.started => self.done = true,
            Event::StateChanged(SessionState::WaitingForApproval) => {
                self.attention = Some("The session is waiting for tool approval. Open the session to inspect and respond.".into());
            }
            Event::StateChanged(SessionState::Running | SessionState::ToolRunning) => {
                self.attention = None
            }
            Event::StateChanged(SessionState::Terminated) => {
                self.failure
                    .get_or_insert("The session was terminated".into());
                self.done = true;
            }
            Event::Error(error) => self.failure = Some(error.message),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::state_with_rig_config;
    use horizon_agent::contract::Message;

    #[test]
    fn initialization_idle_is_not_assignment_completion_and_approval_stays_visible() {
        let state = state_with_rig_config(false, "test");
        let session = SessionId::new();
        let subscription = state.subscribe_to_session(session);
        let mut monitor = Monitor::new(1, "token".into(), subscription);
        monitor.observe(Event::StateChanged(SessionState::WaitingForUser));
        assert!(!monitor.done);
        monitor.observe(Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: "assignment".into(),
        }));
        monitor.observe(Event::StateChanged(SessionState::WaitingForApproval));
        assert!(monitor.attention.is_some());
        assert!(!monitor.done);
        monitor.observe(Event::StateChanged(SessionState::ToolRunning));
        assert!(monitor.attention.is_none());
        monitor.observe(Event::StateChanged(SessionState::WaitingForUser));
        assert!(monitor.done);
    }
}
