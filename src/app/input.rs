use floem::action::{set_ime_allowed, set_ime_cursor_area};
use floem::event::{Event, EventPropagation};
use floem::prelude::*;

use crate::control_surface::{handle_control_key, open_palette, ControlMode};
use crate::input::is_palette_open_key;
use crate::terminal::TerminalCommand;
use crate::workspace::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane, trace_ime,
};

use super::context::{control_input_state, open_palette_state};
use super::state::AppState;

#[derive(Clone)]
pub(super) struct AppInput {
    state: AppState,
}

impl AppInput {
    pub(super) fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
        }
    }

    pub(super) fn handle_window_focus(&self) -> EventPropagation {
        set_ime_allowed(active_text_input_pane(self.state.workspace));
        let (position, size) = self.state.ime_cursor_area.get_untracked();
        set_ime_cursor_area(position, size);
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_enabled(&self) -> EventPropagation {
        trace_ime("enabled");
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_disabled(&self) -> EventPropagation {
        trace_ime("disabled");
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_preedit(&self, event: &Event) -> EventPropagation {
        if !active_text_input_pane(self.state.workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImePreedit { text, cursor } = event {
            let (position, size) = self.state.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("preedit text={text:?} cursor={cursor:?}"));
            if text.is_empty() {
                self.state.ime_composing.set(false);
                self.state.ime_preedit.set(None);
            } else {
                self.state.ime_composing.set(true);
                self.state.ime_preedit.set(Some(text.clone()));
            }
            return EventPropagation::Stop;
        }

        EventPropagation::Continue
    }

    pub(super) fn handle_ime_commit(&self, event: &Event) -> EventPropagation {
        if !active_text_input_pane(self.state.workspace) {
            return EventPropagation::Continue;
        }

        if let Event::ImeCommit(text) = event {
            let (position, size) = self.state.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("commit text={text:?}"));
            self.state.ime_composing.set(false);
            self.state.ime_preedit.set(None);
            if active_agent(self.state.workspace) {
                if let Some(draft) =
                    active_agent_draft(self.state.workspace, self.state.agent_drafts)
                {
                    draft.update(|draft| draft.push_str(text));
                    return EventPropagation::Stop;
                }
            }
            if let Some(tx) = active_terminal_sender(self.state.workspace, self.state.sessions) {
                let _ = tx.send(TerminalCommand::Input(text.as_bytes().to_vec()));
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    }

    pub(super) fn handle_key_down(&self, event: &Event) -> EventPropagation {
        if let Event::KeyDown(key_event) = event {
            if self.state.palette_open.get_untracked()
                && handle_control_key(key_event, control_input_state(&self.state))
            {
                return EventPropagation::Stop;
            }

            if is_palette_open_key(key_event) {
                self.state.ime_composing.set(false);
                self.state.ime_preedit.set(None);
                set_ime_allowed(false);
                self.state.control_mode.set(ControlMode::Commands);
                open_palette(open_palette_state(&self.state));
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    }
}
