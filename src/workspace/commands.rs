//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;

use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::CloseCursorOutcome;
use horizon_workspace::commands::{CommandId, CommandState};

use super::{Modal, Placement, Shell, plugins};

impl Shell {
    /// What the commands' availability is decided from.
    pub(super) fn command_state(&self) -> CommandState {
        self.model.with_untracked(|model| CommandState {
            tab_count: model.tab_count(),
            visible_pane_count: model.visible_panes().len(),
            has_active_session: model.active_session_id().is_some(),
            detached_session_count: model.detached_session_count(),
            has_active_session_directory: (model.active_session_id())
                .is_some_and(|session| model.session_directory(session).is_some()),
        })
    }

    pub fn execute(&self, command: CommandId) {
        let kept = self.kept();
        match command {
            CommandId::NewTab => self.place(Placement::NewTab),
            CommandId::SplitRight => self.place(Placement::SplitRight),
            CommandId::SplitDown => self.place(Placement::SplitDown),
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
            CommandId::OpenBoard => self.create_plugin(plugins::list(), Placement::NewTab),
            CommandId::OpenSessionManager => self.open(Modal::Sessions),
            // A file that cannot be read leaves what is in use as it is.
            CommandId::ReloadConfig => match kept.reload.as_ref().map(|reload| reload()) {
                Some(Ok(config)) => self.apply_config(&config),
                Some(Err(error)) => eprintln!("horizon config: reload failed: {error}"),
                None => {}
            },
            CommandId::ReloadAgentRuntime => self.reload_agent_runtime(),
            CommandId::ReloadTerminalRuntime => self.reload_terminal_runtime(),
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
