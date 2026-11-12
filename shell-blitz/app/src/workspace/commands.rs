//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;

use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::commands::{CommandId, CommandState};
use horizon_workspace::{CloseCursorOutcome, SessionId, SessionKind, SplitAxis};

use super::{Modal, Shell, Start};

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
    CommandId::OpenSessionManager,
    CommandId::OpenTerminalInSessionDirectory,
];

impl Shell {
    /// What the commands' availability is decided from.
    pub(super) fn command_state(&self) -> CommandState {
        self.model.with_untracked(|model| CommandState {
            tab_count: model.tab_count(),
            visible_pane_count: model.visible_panes().len(),
            has_active_session: model.active_session_id().is_some(),
            has_cursor_board: false,
            detached_session_count: model.detached_session_count(),
            has_pending_approval: false,
            has_turn_in_flight: false,
            has_paused_turn: false,
            has_active_session_directory: (model.active_session_id())
                .is_some_and(|session| model.session_directory(session).is_some()),
        })
    }

    pub fn execute(&self, command: CommandId) {
        let kept = self.kept();
        // A new terminal's shell starts where the one it was made from is.
        let begin = |made: SessionId, from: Option<SessionId>| {
            if let Some(from) = from {
                (kept.starting.borrow_mut()).insert(made, Start::Beside(from));
            }
        };
        match command {
            CommandId::NewTab => self.change(|model| {
                let from = model.active_terminal_session_id();
                model.exit_workspace_mode();
                let made = model.open_tab_with_new_session_activated(SessionKind::Terminal, true);
                begin(made, from);
            }),
            CommandId::SplitRight | CommandId::SplitDown => {
                let axis = match command {
                    CommandId::SplitRight => SplitAxis::Horizontal,
                    _ => SplitAxis::Vertical,
                };
                self.change(|model| {
                    let from = model.active_terminal_session_id();
                    if let Some(target) = model.active_session_id() {
                        let kind = SessionKind::Terminal;
                        if let Some(made) =
                            model.split_session_with_new_session(target, kind, axis, true)
                        {
                            begin(made, from);
                        }
                    }
                    model.exit_workspace_mode();
                });
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
            CommandId::OpenSessionManager => self.open(Modal::Sessions),
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
