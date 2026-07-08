use floem::action::{set_ime_allowed, set_ime_cursor_area};
use floem::event::{Event, EventPropagation};
use floem::prelude::*;

use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::app::keymap::{is_palette_open_key, is_workspace_mode_enter_key, Keymap};
use crate::control_surface::{handle_control_key, open_palette};
use crate::terminal::TerminalCommand;
use crate::workspace::{
    active_agent, active_agent_draft, active_terminal_sender, active_text_input_pane,
    agent_escape_requests_workspace_mode, handle_workspace_mode_key, insert_agent_draft_text,
    trace_ime, ModeAction,
};

use super::context::{control_input_state, open_palette_state, session_manager_handle};
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
        self.state.window_focused.set(true);
        set_ime_allowed(active_text_input_pane(self.state.workspace));
        let (position, size) = self.state.ime_cursor_area.get_untracked();
        set_ime_cursor_area(position, size);
        EventPropagation::Continue
    }

    /// Counterpart to [`Self::handle_window_focus`] -- floem's
    /// `WindowLostFocus` (`docs/tasks/backlog.md` item 5). Only updates
    /// `window_focused`; unlike gaining focus, losing it doesn't need to
    /// touch IME state (a composition in progress survives an alt-tab and
    /// resumes when the window is focused again).
    pub(super) fn handle_window_lost_focus(&self) -> EventPropagation {
        self.state.window_focused.set(false);
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_enabled(&self) -> EventPropagation {
        trace_ime("enabled");
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_disabled(&self) -> EventPropagation {
        trace_ime("disabled");
        // Per winit's `Ime::Disabled` contract, no more `Preedit`/`Commit`
        // events arrive until the next `Enabled` -- so if composition was
        // still in progress (window lost IME focus mid-composition, e.g. an
        // alt-tab, without an intervening pane-level `FocusLost`), nothing
        // else will ever clear these. Clearing here as well as on Commit
        // and on an empty Preedit (see below) closes that gap; see
        // `workspace::input`'s composing guard for what a stuck
        // `ime_composing` costs -- every later Character key swallowed.
        self.state.ime_composing.set(false);
        self.state.ime_preedit.set(None);
        EventPropagation::Continue
    }

    pub(super) fn handle_ime_preedit(&self, event: &Event) -> EventPropagation {
        if let Event::ImePreedit { text, cursor } = event {
            let (position, size) = self.state.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("preedit text={text:?} cursor={cursor:?}"));

            if text.is_empty() {
                // Cleared unconditionally, regardless of which pane is
                // currently active: an empty Preedit always ends
                // composition, and skipping this because
                // `active_text_input_pane` happens to read false at this
                // instant would leave `ime_composing` stuck (see
                // `handle_ime_commit`, which clears the same way).
                self.state.ime_composing.set(false);
                self.state.ime_preedit.set(None);
                return EventPropagation::Stop;
            }

            if !active_text_input_pane(self.state.workspace) {
                return EventPropagation::Continue;
            }

            self.state.ime_composing.set(true);
            self.state.ime_preedit.set(Some(text.clone()));
            return EventPropagation::Stop;
        }

        EventPropagation::Continue
    }

    pub(super) fn handle_ime_commit(&self, event: &Event) -> EventPropagation {
        if let Event::ImeCommit(text) = event {
            let (position, size) = self.state.ime_cursor_area.get_untracked();
            set_ime_cursor_area(position, size);
            trace_ime(&format!("commit text={text:?}"));
            // Cleared unconditionally, before the active-pane check below:
            // a Commit always ends composition, even if it arrives while
            // the active pane momentarily doesn't accept text input --
            // otherwise `ime_composing` is left permanently `true`,
            // swallowing every later Character key
            // (`workspace::input::handle_active_pane_key`'s composing
            // guard) with no way back short of a pane focus change.
            self.state.ime_composing.set(false);
            self.state.ime_preedit.set(None);

            if !active_text_input_pane(self.state.workspace) {
                return EventPropagation::Continue;
            }

            if active_agent(self.state.workspace) {
                if let Some(draft) =
                    active_agent_draft(self.state.workspace, self.state.agent_drafts.clone())
                {
                    insert_agent_draft_text(draft, text);
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
                open_palette(open_palette_state(&self.state));
                return EventPropagation::Stop;
            }

            // Workspace mode (`docs/workspace-mode-design.md`) -- this
            // mirrors the same check `workspace::view::pane`'s own
            // `KeyDown` handler runs (and, like the `is_palette_open_key`
            // duplication right above, exists here too as a fallback for
            // whenever no pane currently holds real keyboard focus, e.g.
            // right after a non-refocusing palette command closes the
            // palette -- see `docs/workspace-mode-design.md`'s "everything
            // else restores").
            if is_workspace_mode_enter_key(key_event) {
                if !self
                    .state
                    .workspace
                    .with_untracked(|ws| ws.is_workspace_mode_active())
                {
                    execute_command(
                        CommandInvocation::EnterWorkspaceMode,
                        self.command_action_state(),
                    );
                }
                return EventPropagation::Stop;
            }

            if self
                .state
                .workspace
                .with_untracked(|ws| ws.is_workspace_mode_active())
            {
                if let Some(action) = handle_workspace_mode_key(
                    key_event,
                    self.state.ime_composing,
                    self.state.ime_preedit,
                ) {
                    self.dispatch_workspace_mode_action(action);
                }
                return EventPropagation::Stop;
            }

            if active_agent(self.state.workspace)
                && agent_escape_requests_workspace_mode(
                    key_event,
                    self.state.ime_composing.get_untracked(),
                )
            {
                execute_command(
                    CommandInvocation::EnterWorkspaceMode,
                    self.command_action_state(),
                );
                return EventPropagation::Stop;
            }

            // Config-driven command keybindings (`[keybindings]`, see
            // `app::keymap::Keymap`) — checked last, as a fallback under
            // whatever the palette itself didn't already claim. Note this
            // only ever sees a key that the active pane's own handler left
            // unclaimed: a terminal pane in particular treats most Ctrl/Alt
            // combinations as control input to the shell and consumes them
            // before they can bubble up here (see `workspace::input::
            // handle_terminal_key`) — the same reason only `Ctrl+P` above is
            // special-cased at every layer today.
            if let Some(command_id) = Keymap::global().command_for(key_event) {
                execute_command(
                    CommandInvocation::Simple(command_id),
                    self.command_action_state(),
                );
                return EventPropagation::Stop;
            }
        }

        EventPropagation::Continue
    }

    fn command_action_state(&self) -> CommandActionState {
        CommandActionState {
            runtime: self.state.session_runtime_state(),
            pane_focus_requests: self.state.pane_focus_requests.clone(),
            session_manager: session_manager_handle(&self.state),
            palette: open_palette_state(&self.state),
        }
    }

    /// Dispatches one workspace-mode key action through the command model
    /// -- shared by this file's fallback `KeyDown` handling above and
    /// `workspace::view::pane`'s own per-pane handler, which matches on the
    /// same `ModeAction` the same way.
    fn dispatch_workspace_mode_action(&self, action: ModeAction) {
        match action {
            ModeAction::Move(direction) => execute_command(
                CommandInvocation::MoveWorkspaceCursor { direction },
                self.command_action_state(),
            ),
            ModeAction::Commit => execute_command(
                CommandInvocation::CommitWorkspaceMode,
                self.command_action_state(),
            ),
            ModeAction::Cancel => execute_command(
                CommandInvocation::CancelWorkspaceMode,
                self.command_action_state(),
            ),
            ModeAction::OpenPalette => {
                self.state.ime_composing.set(false);
                self.state.ime_preedit.set(None);
                set_ime_allowed(false);
                open_palette(open_palette_state(&self.state));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preedit(text: &str) -> Event {
        Event::ImePreedit {
            text: text.to_string(),
            cursor: None,
        }
    }

    fn commit(text: &str) -> Event {
        Event::ImeCommit(text.to_string())
    }

    /// Regression coverage for the "Backspace stops working after typing
    /// Japanese" report: whatever the exact platform ordering, `Commit`
    /// must always leave `ime_composing`/`ime_preedit` clear -- that's the
    /// same condition `workspace::input`'s composing guard checks before
    /// swallowing a Character key, and the same signal
    /// `workspace::view::pane` uses to draw the preedit overlay over the
    /// terminal, so a stuck `Some(..)` here reads as "nothing I type does
    /// anything" even when the terminal itself is fine underneath.
    #[test]
    fn commit_clears_composing_without_a_preceding_empty_preedit() {
        let state = AppState::for_test();
        let input = AppInput::new(&state);

        input.handle_ime_preedit(&preedit("か"));
        input.handle_ime_preedit(&preedit("かa"));
        assert!(
            state.ime_composing.get_untracked(),
            "composing while typing"
        );
        assert_eq!(state.ime_preedit.get_untracked(), Some("かa".to_string()));

        // Some IMEs/platforms commit directly, with no empty Preedit at
        // all before or after.
        input.handle_ime_commit(&commit("漢字"));

        assert!(!state.ime_composing.get_untracked());
        assert_eq!(state.ime_preedit.get_untracked(), None);
    }

    #[test]
    fn commit_clears_composing_with_a_leading_empty_preedit() {
        let state = AppState::for_test();
        let input = AppInput::new(&state);

        input.handle_ime_preedit(&preedit("か"));
        input.handle_ime_preedit(&preedit("かa"));
        // winit's documented contract for this pinned rev: "Right before
        // [Commit], winit will send empty Preedit."
        input.handle_ime_preedit(&preedit(""));
        assert!(
            !state.ime_composing.get_untracked(),
            "an empty Preedit alone must already end composition"
        );

        input.handle_ime_commit(&commit("漢字"));

        assert!(!state.ime_composing.get_untracked());
        assert_eq!(state.ime_preedit.get_untracked(), None);
    }

    #[test]
    fn commit_clears_composing_with_a_trailing_empty_preedit() {
        let state = AppState::for_test();
        let input = AppInput::new(&state);

        input.handle_ime_preedit(&preedit("か"));
        input.handle_ime_preedit(&preedit("かa"));
        input.handle_ime_commit(&commit("漢字"));
        assert!(!state.ime_composing.get_untracked());

        // Some platforms/IMEs send the empty Preedit *after* Commit
        // instead of before.
        input.handle_ime_preedit(&preedit(""));

        assert!(!state.ime_composing.get_untracked());
        assert_eq!(state.ime_preedit.get_untracked(), None);
    }

    #[test]
    fn ime_disabled_clears_a_stuck_composing_flag() {
        // Per winit's contract, `Ime::Disabled` means no more Preedit or
        // Commit events until the next `Enabled` -- so if the IME is
        // disabled mid-composition (e.g. the window loses IME focus
        // without an intervening pane-level `FocusLost`), nothing else
        // will ever clear `ime_composing`/`ime_preedit` again.
        let state = AppState::for_test();
        let input = AppInput::new(&state);

        input.handle_ime_preedit(&preedit("か"));
        assert!(state.ime_composing.get_untracked());

        input.handle_ime_disabled();

        assert!(!state.ime_composing.get_untracked());
        assert_eq!(state.ime_preedit.get_untracked(), None);
    }
}
