use std::collections::HashMap;
use std::time::Instant;

use crate::agent::frame::{AgentFrame, StateEntry};
use crate::session::SessionId;
use crate::terminal::{initial_terminal_text, TerminalFrame};

#[derive(Clone, Debug, Default)]
pub(crate) struct Frames {
    terminal: HashMap<SessionId, TerminalFrame>,
    agent: HashMap<SessionId, AgentFrame>,
    // How long each agent session has held its current `AgentFrame.state` —
    // `AgentFrame` itself can't carry this (see `StateEntry`'s doc comment),
    // so it's kept alongside as a sidecar, updated every time a fresh frame
    // comes in. This is what pane headers read to show elapsed time in the
    // current state (`docs/ux-principles.md`'s Persistent UI Requirement to
    // show pane state).
    agent_state_entries: HashMap<SessionId, StateEntry>,
}

impl Frames {
    pub(crate) fn terminal_frame(&self, session_id: SessionId) -> TerminalFrame {
        self.terminal
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| TerminalFrame::from_text(initial_terminal_text()))
    }

    pub(crate) fn update_terminal_output(&mut self, session_id: SessionId, output: String) {
        self.update_terminal_frame(session_id, TerminalFrame::from_text(output));
    }

    pub(crate) fn update_terminal_frame(&mut self, session_id: SessionId, frame: TerminalFrame) {
        self.terminal.insert(session_id, frame);
    }

    pub(crate) fn agent_frame(&self, session_id: SessionId) -> AgentFrame {
        self.agent
            .get(&session_id)
            .cloned()
            .unwrap_or_else(AgentFrame::empty)
    }

    pub(crate) fn update_agent_frame(&mut self, session_id: SessionId, frame: AgentFrame) {
        let entry = self
            .agent_state_entries
            .get(&session_id)
            .copied()
            .unwrap_or_else(|| StateEntry::initial(frame.state));
        self.agent_state_entries
            .insert(session_id, entry.advance(frame.state));
        self.agent.insert(session_id, frame);
    }

    /// When the visible agent session's `AgentFrame.state` last changed —
    /// `None` if the session has never had a frame recorded.
    pub(crate) fn agent_state_entered_at(&self, session_id: SessionId) -> Option<Instant> {
        self.agent_state_entries
            .get(&session_id)
            .map(StateEntry::entered_at)
    }

    pub(crate) fn remove_session(&mut self, session_id: SessionId) {
        self.terminal.remove(&session_id);
        self.agent.remove(&session_id);
        self.agent_state_entries.remove(&session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Message, MessageRole};

    #[test]
    fn terminal_frame_defaults_to_initial_terminal_text() {
        let frames = Frames::default();
        let frame = frames.terminal_frame(SessionId::new());

        assert!(frame.text.contains("Terminal plugin"));
    }

    #[test]
    fn terminal_output_updates_frame_by_session() {
        let session_id = SessionId::new();
        let mut frames = Frames::default();

        frames.update_terminal_output(session_id, "Terminal exited".to_string());

        assert_eq!(frames.terminal_frame(session_id).text, "Terminal exited");
    }

    #[test]
    fn agent_frame_defaults_empty_and_updates_by_session() {
        let session_id = SessionId::new();
        let mut frames = Frames::default();
        assert_eq!(frames.agent_frame(session_id), AgentFrame::empty());

        let frame = AgentFrame {
            state: None,
            items: vec![crate::agent::frame::AgentFrameItem::Message(Message {
                role: MessageRole::Assistant,
                text: "hello".to_string(),
            })],
        };
        frames.update_agent_frame(session_id, frame.clone());

        assert_eq!(frames.agent_frame(session_id), frame);
    }

    #[test]
    fn agent_state_entered_at_resets_only_on_state_change() {
        use crate::agent::contract::SessionState;

        let session_id = SessionId::new();
        let mut frames = Frames::default();
        assert_eq!(frames.agent_state_entered_at(session_id), None);

        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::Running),
                items: Vec::new(),
            },
        );
        let first_entered_at = frames
            .agent_state_entered_at(session_id)
            .expect("entry recorded for a session with a state");

        // Re-observing the same state must not reset the timestamp.
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::Running),
                items: vec![crate::agent::frame::AgentFrameItem::Message(Message {
                    role: MessageRole::Assistant,
                    text: "still running".to_string(),
                })],
            },
        );
        assert_eq!(
            frames.agent_state_entered_at(session_id),
            Some(first_entered_at)
        );

        // A genuine state transition must produce a fresh timestamp.
        std::thread::sleep(std::time::Duration::from_millis(5));
        frames.update_agent_frame(
            session_id,
            AgentFrame {
                state: Some(SessionState::WaitingForUser),
                items: Vec::new(),
            },
        );
        let second_entered_at = frames
            .agent_state_entered_at(session_id)
            .expect("entry still recorded after a state change");
        assert!(second_entered_at > first_entered_at);
    }
}
