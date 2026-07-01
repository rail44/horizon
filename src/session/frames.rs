use std::collections::HashMap;

use crate::agent::frame::AgentFrame;
use crate::session::SessionId;
use crate::terminal::{initial_terminal_text, TerminalFrame};

#[derive(Clone, Debug, Default)]
pub struct Frames {
    terminal: HashMap<SessionId, TerminalFrame>,
    agent: HashMap<SessionId, AgentFrame>,
}

impl Frames {
    pub fn terminal_frame(&self, session_id: SessionId) -> TerminalFrame {
        self.terminal
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| TerminalFrame::from_text(initial_terminal_text()))
    }

    pub fn update_terminal_output(&mut self, session_id: SessionId, output: String) {
        self.update_terminal_frame(session_id, TerminalFrame::from_text(output));
    }

    pub fn update_terminal_frame(&mut self, session_id: SessionId, frame: TerminalFrame) {
        self.terminal.insert(session_id, frame);
    }

    pub fn agent_frame(&self, session_id: SessionId) -> AgentFrame {
        self.agent
            .get(&session_id)
            .cloned()
            .unwrap_or_else(AgentFrame::empty)
    }

    pub fn update_agent_frame(&mut self, session_id: SessionId, frame: AgentFrame) {
        self.agent.insert(session_id, frame);
    }

    pub fn remove_session(&mut self, session_id: SessionId) {
        self.terminal.remove(&session_id);
        self.agent.remove(&session_id);
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
}
