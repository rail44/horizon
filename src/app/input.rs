use std::path::PathBuf;

use floem::action::{set_ime_allowed, set_ime_cursor_area};
use floem::event::{Event, EventPropagation};
use floem::prelude::*;

use crate::agent_config::AgentConfig;
use crate::app::commands::{active_agent, active_text_input_pane, PaneFocusRequests};
use crate::control_surface::{handle_control_key, open_palette, ControlMode};
use crate::input::is_palette_open_key;
use crate::session::{Frames, Registry};
use crate::terminal::TerminalCommand;
use crate::workspace::{
    active_agent_draft, active_terminal_sender, trace_ime, AgentDrafts, Workspace,
};

#[derive(Clone)]
pub struct AppInput {
    pub workspace: RwSignal<Workspace>,
    pub frames: RwSignal<Frames>,
    pub sessions: RwSignal<Registry>,
    pub ime_composing: RwSignal<bool>,
    pub ime_preedit: RwSignal<Option<String>>,
    pub ime_cursor_area: RwSignal<(floem::peniko::kurbo::Point, floem::peniko::kurbo::Size)>,
    pub palette_open: RwSignal<bool>,
    pub palette_query: RwSignal<String>,
    pub palette_selection: RwSignal<usize>,
    pub palette_focus_request: RwSignal<u64>,
    pub pane_focus_requests: PaneFocusRequests,
    pub agent_drafts: AgentDrafts,
    pub control_mode: RwSignal<ControlMode>,
    pub overview_selection: RwSignal<usize>,
    pub agent_state_status: RwSignal<Option<String>>,
    pub agent_config: AgentConfig,
    pub terminal_dump: Option<PathBuf>,
    pub clipboard_dump: Option<PathBuf>,
}

impl AppInput {
    pub fn handle_window_focus(&self) -> EventPropagation {
        set_ime_allowed(active_text_input_pane(self.workspace));
        let (position, size) = self.ime_cursor_area.get_untracked();
        set_ime_cursor_area(position, size);
        EventPropagation::Continue
    }

    pub fn handle_ime_enabled(&self) -> EventPropagation {
        trace_ime("enabled");
        EventPropagation::Continue
    }

    pub fn handle_ime_disabled(&self) -> EventPropagation {
        trace_ime("disabled");
        EventPropagation::Continue
    }

    pub fn handle_ime_preedit(&self, event: &Event) -> EventPropagation {
        if !active_text_input_pane(self.workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImePreedit { text, cursor } = event {
            let (position, size) = self.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("preedit text={text:?} cursor={cursor:?}"));
            if text.is_empty() {
                self.ime_composing.set(false);
                self.ime_preedit.set(None);
            } else {
                self.ime_composing.set(true);
                self.ime_preedit.set(Some(text.clone()));
            }
            return EventPropagation::Stop;
        }

        EventPropagation::Continue
    }

    pub fn handle_ime_commit(&self, event: &Event) -> EventPropagation {
        if !active_text_input_pane(self.workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImeCommit(text) = event {
            let (position, size) = self.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("commit text={text:?}"));
            self.ime_composing.set(false);
            self.ime_preedit.set(None);
            if active_agent(self.workspace) {
                if let Some(draft) = active_agent_draft(self.workspace, self.agent_drafts) {
                    draft.update(|draft| draft.push_str(text));
                    return EventPropagation::Stop;
                }
            }
            if let Some(tx) = active_terminal_sender(self.workspace, self.sessions) {
                let _ = tx.send(TerminalCommand::Input(text.as_bytes().to_vec()));
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    }

    pub fn handle_key_down(&self, event: &Event) -> EventPropagation {
        if let Event::KeyDown(key_event) = event {
            if self.palette_open.get_untracked()
                && handle_control_key(
                    key_event,
                    self.workspace,
                    self.frames,
                    self.sessions,
                    self.palette_open,
                    self.palette_query,
                    self.palette_selection,
                    self.control_mode,
                    self.overview_selection,
                    self.pane_focus_requests,
                    self.agent_state_status,
                    self.agent_config.clone(),
                    self.terminal_dump.clone(),
                    self.clipboard_dump.clone(),
                )
            {
                return EventPropagation::Stop;
            }

            if is_palette_open_key(key_event) {
                self.ime_composing.set(false);
                self.ime_preedit.set(None);
                set_ime_allowed(false);
                self.control_mode.set(ControlMode::Commands);
                open_palette(
                    self.palette_open,
                    self.palette_query,
                    self.palette_selection,
                    self.palette_focus_request,
                );
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    }
}
