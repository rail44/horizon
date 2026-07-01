use std::collections::HashMap;

use crossbeam_channel::Sender;

use crate::agent::contract::{Command, SessionHandle};
use crate::session::SessionId;
use crate::terminal::TerminalCommand;

#[derive(Clone, Default)]
pub struct Registry {
    terminals: HashMap<SessionId, Sender<TerminalCommand>>,
    agents: HashMap<SessionId, SessionHandle>,
}

impl Registry {
    pub fn insert_terminal(&mut self, session_id: SessionId, sender: Sender<TerminalCommand>) {
        self.terminals.insert(session_id, sender);
    }

    pub fn terminal_sender(&self, session_id: SessionId) -> Option<Sender<TerminalCommand>> {
        self.terminals.get(&session_id).cloned()
    }

    pub fn shutdown_terminal(&mut self, session_id: SessionId) -> bool {
        let Some(sender) = self.terminals.remove(&session_id) else {
            return false;
        };
        let _ = sender.send(TerminalCommand::Shutdown);
        true
    }

    pub fn insert_agent(&mut self, session_id: SessionId, handle: SessionHandle) {
        self.agents.insert(session_id, handle);
    }

    pub fn agent_sender(&self, session_id: SessionId) -> Option<Sender<Command>> {
        self.agents.get(&session_id).map(SessionHandle::sender)
    }

    pub fn shutdown_agent(&mut self, session_id: SessionId) -> bool {
        let Some(handle) = self.agents.remove(&session_id) else {
            return false;
        };
        let _ = handle.sender().send(Command::Shutdown);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_removes_agent_session() {
        let session_id = SessionId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let (_events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut registry = Registry::default();
        registry.insert_agent(session_id, SessionHandle::new(tx, events_rx));

        assert!(registry.agent_sender(session_id).is_some());
        assert!(registry.shutdown_agent(session_id));
        assert!(registry.agent_sender(session_id).is_none());
        assert!(matches!(rx.try_recv(), Ok(Command::Shutdown)));
    }
}
