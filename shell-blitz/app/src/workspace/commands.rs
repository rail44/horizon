//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;

use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::agent_model::ToolCallIdentity;
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::CloseCursorOutcome;
use horizon_workspace::commands::{CommandId, CommandState};

use super::{Modal, Placement, Shell};
use crate::agent::AgentSession;

/// The commands of the model this shell carries out so far: what the
/// palette lists.
pub(super) const CARRIED_OUT: &[CommandId] = &[
    CommandId::SplitRight,
    CommandId::SplitDown,
    CommandId::NewTab,
    CommandId::FocusNextPane,
    CommandId::CloseActivePane,
    CommandId::CloseActiveTab,
    CommandId::TerminateActiveSession,
    CommandId::TerminateAllDetachedSessions,
    CommandId::ApproveToolCall,
    CommandId::DenyToolCall,
    CommandId::CancelAgentTurn,
    CommandId::ContinueAgentTurn,
    CommandId::OpenSessionManager,
    CommandId::OpenTerminalInSessionDirectory,
    CommandId::ReloadConfig,
    CommandId::IncreaseFontSize,
    CommandId::DecreaseFontSize,
    CommandId::ResetFontSize,
];

impl Shell {
    /// What the commands' availability is decided from.
    pub(super) fn command_state(&self) -> CommandState {
        let standing = (self.active_agent())
            .map(|session| session.standing())
            .unwrap_or_default();
        self.model.with_untracked(|model| CommandState {
            tab_count: model.tab_count(),
            visible_pane_count: model.visible_panes().len(),
            has_active_session: model.active_session_id().is_some(),
            has_cursor_board: false,
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
            CommandId::OpenSessionManager => self.open(Modal::Sessions),
            // A file that cannot be read leaves what is in use as it is.
            CommandId::ReloadConfig => match kept.reload.as_ref().map(|reload| reload()) {
                Some(Ok(config)) => self.apply_config(&config),
                Some(Err(error)) => eprintln!("horizon config: reload failed: {error}"),
                None => {}
            },
            CommandId::IncreaseFontSize => self.step_font_size(1.),
            CommandId::DecreaseFontSize => self.step_font_size(-1.),
            CommandId::ResetFontSize => {
                let size = kept.configured_size.get();
                self.theme.update(|theme| theme.font_size = size);
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
            _ => {}
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
