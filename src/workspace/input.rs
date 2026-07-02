use crate::agent::contract::Command;
use crate::app::commands::MAX_VISIBLE_PANES;
use crate::input::{
    agent_draft_action, is_terminal_copy_key, is_terminal_paste_key, pop_last_grapheme_approx,
    terminal_input_from_key, terminal_key_from_key, termwiz_modifiers, AgentDraftAction,
};
use crate::session::Registry;
use crate::terminal::TerminalCommand;
use crate::workspace::{PaneKind, Workspace};
use floem::keyboard::{Key, KeyEvent};
use floem::prelude::*;
use floem::Clipboard;

pub type AgentDrafts = [RwSignal<String>; MAX_VISIBLE_PANES];

pub fn active_agent(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_is(PaneKind::Agent))
}

pub fn active_text_input_pane(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_accepts_text_input())
}

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

fn handle_terminal_key(
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

fn handle_agent_key(
    key_event: &KeyEvent,
    draft: RwSignal<String>,
    agent_tx: Option<crossbeam_channel::Sender<Command>>,
) -> bool {
    if is_terminal_paste_key(key_event) {
        if let Ok(text) = Clipboard::get_contents() {
            draft.update(|draft| draft.push_str(&text));
            return true;
        }
    }

    match agent_draft_action(&key_event.key.logical_key, key_event.modifiers) {
        Some(AgentDraftAction::Insert(text)) => {
            draft.update(|draft| draft.push_str(&text));
            true
        }
        Some(AgentDraftAction::Backspace) => {
            draft.update(|draft| {
                pop_last_grapheme_approx(draft);
            });
            true
        }
        Some(AgentDraftAction::Submit) => {
            let text = draft.with_untracked(|draft| draft.trim().to_string());
            if text.is_empty() {
                return true;
            }
            if let Some(tx) = agent_tx {
                let command = Command::UserMessage { text };
                let _ = tx.send(command);
                draft.set(String::new());
            }
            true
        }
        None => false,
    }
}

pub fn handle_active_pane_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
    ime_composing: RwSignal<bool>,
    agent_draft: RwSignal<String>,
) -> bool {
    if ime_composing.get_untracked() && matches!(key_event.key.logical_key, Key::Character(_)) {
        return true;
    }

    if workspace.with(|ws| ws.active_visible_pane_is(index, PaneKind::Agent)) {
        return handle_agent_key(
            key_event,
            agent_draft,
            visible_agent_sender(workspace, sessions, index),
        );
    }

    if workspace.with(|ws| ws.active_visible_pane_is(index, PaneKind::Terminal)) {
        return handle_terminal_key(
            key_event,
            visible_terminal_sender(workspace, sessions, index),
        );
    }

    false
}

pub fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}
