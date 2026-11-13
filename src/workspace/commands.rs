//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;

use horizon_board_view::model::{ListCommand, ThreadCommand};
use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::agent_model::ToolCallIdentity;
use horizon_terminal_core::TerminalCommand;
use horizon_ui::reactive_graph::traits::{GetUntracked, Update, WithUntracked};
use horizon_ui::request_paste;
use horizon_workspace::commands::{CommandId, CommandState};
use horizon_workspace::{CloseCursorOutcome, ViewKind, Workspace};

use super::{Modal, Placement, Shell};
use crate::agent::AgentSession;

impl Shell {
    /// What the commands' availability is decided from.
    pub(super) fn command_state(&self) -> CommandState {
        let standing = (self.active_agent())
            .map(|session| session.standing())
            .unwrap_or_default();
        let on_board = (self.model.with_untracked(Workspace::cursor_pane_id)).is_some_and(|pane| {
            self.board_list(pane).is_some() || self.board_thread(pane).is_some()
        });
        let kept = self.kept();
        self.model.with_untracked(|model| CommandState {
            tab_count: model.tab_count(),
            visible_pane_count: model.visible_panes().len(),
            has_active_session: model.active_session_id().is_some(),
            has_cursor_board: on_board,
            has_active_terminal: (model.active_session_id())
                .is_some_and(|session| kept.sessions.borrow().contains_key(&session)),
            detached_session_count: model.detached_session_count(),
            has_pending_approval: standing.pending_approval,
            has_turn_in_flight: standing.turn_in_flight,
            has_paused_turn: standing.paused_turn,
            has_active_session_directory: (model.active_session_id())
                .is_some_and(|session| model.session_directory(session).is_some()),
        })
    }

    pub fn execute(&self, command: CommandId) {
        let kept = self.kept();
        // An agent's commands are for the session the cursor is on.
        let agent = |command: fn(&AgentSession) -> Option<AgentCommand>| {
            if let Some(session) = self.active_agent()
                && let Some(command) = command(&session)
            {
                session.send(command);
            }
        };
        fn oldest(session: &AgentSession) -> Option<ToolCallIdentity> {
            session.pending_approvals().into_iter().next()
        }
        match command {
            CommandId::NewTab => self.place(Placement::NewTab),
            CommandId::SplitRight => self.place(Placement::SplitRight),
            CommandId::SplitDown => self.place(Placement::SplitDown),
            CommandId::ApproveToolCall => {
                agent(|session| oldest(session).map(|identity| AgentCommand::Approve { identity }))
            }
            CommandId::DenyToolCall => agent(|session| {
                let identity = oldest(session)?;
                Some(AgentCommand::Deny {
                    identity,
                    reason: None,
                })
            }),
            CommandId::CancelAgentTurn => agent(|_| Some(AgentCommand::Cancel)),
            CommandId::ContinueAgentTurn => agent(|_| Some(AgentCommand::ContinueTurn)),
            CommandId::SwitchModel => {
                let session = self.model.with_untracked(|model| {
                    let session = model.active_session_id()?;
                    kept.agents
                        .borrow()
                        .contains_key(&session)
                        .then_some(session)
                });
                if let Some(session) = session {
                    self.switch_model(session);
                }
            }
            CommandId::FocusNextPane => self.change(|model| model.focus_next()),
            CommandId::CloseActivePane => self.close_pane(),
            CommandId::CloseActiveTab => self.change(|model| {
                model.exit_workspace_mode();
                model.close_active_tab();
            }),
            // The session is the cursor's: ended before the mode, and the
            // cursor with it, is left.
            CommandId::TerminateActiveSession => self.change(|model| {
                model.terminate_active_session();
                model.exit_workspace_mode();
            }),
            CommandId::TerminateAllDetachedSessions => self.change(|model| {
                for session in model.detached_session_summaries() {
                    model.terminate_session(session.id);
                }
            }),
            CommandId::OpenBoard => self.create_view(ViewKind::BoardList, Placement::NewTab),
            // What the board's own views carry out, each for the pane
            // the cursor is on.
            CommandId::ToggleBoardExpansion
            | CommandId::ToggleBoardFinishedBand
            | CommandId::AddBoardTask
            | CommandId::MoveBoardTaskUp
            | CommandId::MoveBoardTaskDown
            | CommandId::ReorderBoardTask => {
                let Some((_, list)) = self.active_board_list() else {
                    return;
                };
                let selected = list.selected.get_untracked();
                let asked = match command {
                    CommandId::ToggleBoardExpansion => selected.map(ListCommand::ToggleExpansion),
                    CommandId::ToggleBoardFinishedBand => Some(ListCommand::ToggleFinished),
                    CommandId::AddBoardTask => Some(ListCommand::AddTask),
                    CommandId::MoveBoardTaskUp => Some(ListCommand::MoveUp),
                    CommandId::MoveBoardTaskDown => Some(ListCommand::MoveDown),
                    // (A drop carries its own move out where it lands.)
                    _ => Some(ListCommand::Reorder),
                };
                if let Some(asked) = asked {
                    list.execute(asked);
                }
            }
            CommandId::PostBoardMessage
            | CommandId::ToggleBoardClosed
            | CommandId::SaveBoardState
            | CommandId::OpenBoardTaskSession => {
                let Some(thread) = self.active_board_thread() else {
                    return;
                };
                thread.execute(match command {
                    CommandId::PostBoardMessage => ThreadCommand::PostMessage,
                    CommandId::ToggleBoardClosed => ThreadCommand::ToggleClosed,
                    CommandId::SaveBoardState => ThreadCommand::SaveStatus,
                    _ => ThreadCommand::OpenTaskSession,
                });
            }
            CommandId::OpenBoardRelatedItem => {
                if let Some((pane, list)) = self.active_board_list()
                    && let Some(task) = list.selected.get_untracked()
                {
                    self.open_thread(pane, task);
                }
            }
            CommandId::BackBoardList => self.back_to_board_list(),
            CommandId::OpenSessionManager => self.open(Modal::Sessions),
            // A file that cannot be read leaves what is in use as it is.
            CommandId::ReloadConfig => match kept.reload.as_ref().map(|reload| reload()) {
                Some(Ok(config)) => self.apply_config(&config),
                Some(Err(error)) => eprintln!("horizon config: reload failed: {error}"),
                None => {}
            },
            CommandId::OpenBoardOrganizer => self.open_board_organizer(),
            CommandId::ReloadAgentRuntime => self.reload_agent_runtime(),
            CommandId::ReloadTerminalRuntime => self.reload_terminal_runtime(),
            CommandId::IncreaseFontSize => self.step_font_size(1.),
            CommandId::DecreaseFontSize => self.step_font_size(-1.),
            CommandId::ResetFontSize => {
                let size = kept.configured_size.get();
                self.theme.update(|theme| theme.font_size = size);
            }
            CommandId::CopyTerminalSelection => {
                let session = self.model.with_untracked(Workspace::active_session_id);
                let session = session.and_then(|id| kept.sessions.borrow().get(&id).cloned());
                if let Some(session) = session {
                    session.send(TerminalCommand::CopySelection);
                }
            }
            // The clipboard's text arrives as a paste, at the view with the
            // keys.
            CommandId::PasteIntoTerminal => {
                if self.command_state().has_active_terminal {
                    request_paste();
                }
            }
            CommandId::ReloadPreview => {
                let pane = self.model.with_untracked(Workspace::cursor_pane_id);
                if let Some(preview) = pane.and_then(|pane| self.preview(pane)) {
                    preview.reload();
                }
            }
            CommandId::OpenTerminalInSessionDirectory => {
                let directory = self.model.with_untracked(|model| {
                    let session = model.active_session_id()?;
                    model.session_directory(session).map(Path::to_path_buf)
                });
                if let Some(directory) = directory {
                    self.open_terminal_in(directory);
                }
            }
        }
    }

    /// Closes the cursor's pane, or its tab when the pane is the tab's
    /// last. The sessions shown there go on: closing detaches them.
    pub(super) fn close_pane(&self) {
        self.change(|model| {
            if !matches!(model.close_cursor_pane_or_tab(), CloseCursorOutcome::Noop) {
                model.exit_workspace_mode();
            }
        });
    }
}
