use std::collections::HashMap;

use crossbeam_channel::Sender;

use crate::terminal::TerminalCommand;
use crate::workspace::SessionId;

#[derive(Clone, Default)]
pub struct SessionRegistry {
    terminals: HashMap<SessionId, Sender<TerminalCommand>>,
}

impl SessionRegistry {
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

    pub fn shutdown_terminal_if_unreferenced(
        &mut self,
        session_id: SessionId,
        is_referenced: bool,
    ) -> bool {
        if is_referenced {
            return false;
        }

        self.shutdown_terminal(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_referenced_terminal_session() {
        let session_id = SessionId::new();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut registry = SessionRegistry::default();
        registry.insert_terminal(session_id, tx);

        assert!(!registry.shutdown_terminal_if_unreferenced(session_id, true));
        assert!(registry.terminal_sender(session_id).is_some());
    }

    #[test]
    fn shutdown_removes_unref_terminal_session() {
        let session_id = SessionId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut registry = SessionRegistry::default();
        registry.insert_terminal(session_id, tx);

        assert!(registry.shutdown_terminal_if_unreferenced(session_id, false));
        assert!(registry.terminal_sender(session_id).is_none());
        assert!(matches!(rx.try_recv(), Ok(TerminalCommand::Shutdown)));
    }
}
