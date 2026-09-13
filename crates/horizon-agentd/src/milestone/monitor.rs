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
    pub(super) successful_checks: Vec<String>,
    pending: Vec<horizon_agent::contract::ToolCallRequest>,
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
            successful_checks: Vec::new(),
            pending: Vec::new(),
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
            Event::ToolCallRequested(request) if self.started && request.tool_id == "bash" => {
                self.pending.push(request)
            }
            Event::ToolCallFinished(result) if self.started => {
                if let Some(index) = self.pending.iter().rposition(|r| {
                    if let Some(occurrence) = &result.occurrence_id {
                        r.occurrence_id.as_ref() == Some(occurrence)
                    } else {
                        r.call_id == result.call_id
                    }
                }) {
                    let request = self.pending.remove(index);
                    if !result.is_error
                        && !result.denied
                        && result.output.get("reused_output").and_then(|v| v.as_bool())
                            != Some(true)
                        && result.output.get("exit_code").and_then(|v| v.as_i64()) == Some(0)
                    {
                        if let Some(command) = request.input.get("command").and_then(|v| v.as_str())
                        {
                            self.successful_checks.push(command.into());
                        }
                    }
                }
            }
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
    fn cached_denied_and_failed_checks_are_not_verification_receipts() {
        use horizon_agent::contract::{ToolCallId, ToolCallRequest, ToolCallResult};
        let state = state_with_rig_config(false, "test");
        let subscription = state.subscribe_to_session(SessionId::new());
        let mut monitor = Monitor::new(1, "token".into(), subscription);
        monitor.observe(Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: "Verify this commit".into(),
        }));
        for (command, output) in [
            (
                "cached",
                serde_json::json!({"exit_code":0,"reused_output":true}),
            ),
            ("failed", serde_json::json!({"exit_code":1})),
            ("denied", serde_json::json!({"exit_code":0,"is_error":true})),
            ("fresh", serde_json::json!({"exit_code":0})),
        ] {
            monitor.observe(Event::ToolCallRequested(ToolCallRequest {
                call_id: ToolCallId(command.into()),
                tool_id: "bash".into(),
                input: serde_json::json!({"command":command}).into(),
                occurrence_id: None,
            }));
            monitor.observe(Event::ToolCallFinished(ToolCallResult::new(
                ToolCallId(command.into()),
                None,
                output,
            )));
        }
        assert_eq!(monitor.successful_checks, vec!["fresh"]);
    }

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
