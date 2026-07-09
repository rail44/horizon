use std::collections::HashMap;

use crossbeam_channel::Sender;

use crate::agent::contract::{Command, SessionHandle};
use crate::session::SessionId;
use crate::terminal::TerminalCommand;

#[derive(Clone, Default)]
pub(crate) struct Registry {
    terminals: HashMap<SessionId, Sender<TerminalCommand>>,
    /// The spawned shell's pid, when the PTY backend reported one
    /// (`TerminalSession::pid`'s doc comment) -- kept separate from
    /// `terminals` rather than folded into a combined value type, since
    /// most reads only ever want the sender. Lets a later-spawned session
    /// sourced from this one sample its *current* cwd on demand
    /// (`Self::terminal_cwd`), per `docs/session-relationship-design.md`'s
    /// "cwd sourcing is shell-independent".
    terminal_pids: HashMap<SessionId, u32>,
    agents: HashMap<SessionId, SessionHandle>,
}

impl Registry {
    pub(crate) fn insert_terminal(
        &mut self,
        session_id: SessionId,
        sender: Sender<TerminalCommand>,
        pid: Option<u32>,
    ) {
        self.terminals.insert(session_id, sender);
        if let Some(pid) = pid {
            self.terminal_pids.insert(session_id, pid);
        }
    }

    pub(crate) fn terminal_sender(&self, session_id: SessionId) -> Option<Sender<TerminalCommand>> {
        self.terminals.get(&session_id).cloned()
    }

    /// Samples `session_id`'s terminal's *current* cwd on demand (not the
    /// cwd it was spawned with -- the shell may have `cd`ed since), via its
    /// retained pid. `None` when the session isn't a known terminal, has no
    /// retained pid, or the live sample itself fails (see
    /// `terminal::sample_cwd`'s doc comment for why those collapse
    /// together).
    pub(crate) fn terminal_cwd(&self, session_id: SessionId) -> Option<std::path::PathBuf> {
        let pid = *self.terminal_pids.get(&session_id)?;
        crate::terminal::sample_cwd(pid)
    }

    pub(crate) fn shutdown_terminal(&mut self, session_id: SessionId) -> bool {
        self.terminal_pids.remove(&session_id);
        let Some(sender) = self.terminals.remove(&session_id) else {
            return false;
        };
        let _ = sender.send(TerminalCommand::Shutdown);
        true
    }

    pub(crate) fn insert_agent(&mut self, session_id: SessionId, handle: SessionHandle) {
        self.agents.insert(session_id, handle);
    }

    pub(crate) fn agent_sender(&self, session_id: SessionId) -> Option<Sender<Command>> {
        self.agents.get(&session_id).map(SessionHandle::sender)
    }

    pub(crate) fn shutdown_agent(&mut self, session_id: SessionId) -> bool {
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
    fn terminal_cwd_samples_the_retained_pid_s_live_cwd() {
        let session_id = SessionId::new();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut registry = Registry::default();
        registry.insert_terminal(session_id, tx, Some(std::process::id()));

        let expected = std::env::current_dir().expect("current dir must be readable in tests");
        assert_eq!(registry.terminal_cwd(session_id), Some(expected));
    }

    #[test]
    fn terminal_cwd_is_none_without_a_retained_pid() {
        let session_id = SessionId::new();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut registry = Registry::default();
        registry.insert_terminal(session_id, tx, None);

        assert!(registry.terminal_cwd(session_id).is_none());
    }

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
