use crate::app::commands::{active_agent, MAX_VISIBLE_PANES};
use crate::session::Registry;
use crate::terminal::TerminalCommand;
use crate::workspace::Workspace;
use floem::prelude::*;

pub type AgentDrafts = [RwSignal<String>; MAX_VISIBLE_PANES];

pub fn active_agent_draft(
    workspace: RwSignal<Workspace>,
    agent_drafts: AgentDrafts,
) -> Option<RwSignal<String>> {
    if !active_agent(workspace) {
        return None;
    }

    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    agent_drafts.get(index).copied()
}

pub fn active_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.active_terminal_session_id())?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}
