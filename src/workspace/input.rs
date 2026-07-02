use crate::agent::contract::Command;
use crate::app::commands::{active_agent, MAX_VISIBLE_PANES};
use crate::input::{
    is_terminal_copy_key, is_terminal_paste_key, terminal_input_from_key, terminal_key_from_key,
    termwiz_modifiers,
};
use crate::session::Registry;
use crate::terminal::TerminalCommand;
use crate::workspace::Workspace;
use floem::keyboard::KeyEvent;
use floem::prelude::*;
use floem::Clipboard;

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

pub fn visible_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_terminal_session_id(index))?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub fn visible_agent_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<Command>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_agent_session_id(index))?;
    sessions.with_untracked(|registry| registry.agent_sender(session_id))
}

pub fn handle_terminal_key(
    key_event: &KeyEvent,
    terminal_tx: Option<crossbeam_channel::Sender<TerminalCommand>>,
) -> bool {
    let Some(tx) = terminal_tx else {
        return false;
    };

    if is_terminal_paste_key(key_event) {
        if let Ok(text) = Clipboard::get_contents() {
            let _ = tx.send(TerminalCommand::Paste(text));
            return true;
        }
    }

    if is_terminal_copy_key(key_event) {
        let _ = tx.send(TerminalCommand::CopySelection);
        return true;
    }

    if let Some(key) = terminal_key_from_key(key_event) {
        let _ = tx.send(TerminalCommand::Key {
            key,
            modifiers: termwiz_modifiers(key_event.modifiers),
            is_down: true,
        });
        return true;
    }

    if let Some(bytes) = terminal_input_from_key(key_event) {
        let _ = tx.send(TerminalCommand::Input(bytes));
        return true;
    }

    false
}

pub fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}
