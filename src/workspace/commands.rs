//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;

use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::commands::{CommandCategory, CommandId, CommandState, command_entries};
use horizon_workspace::{CloseCursorOutcome, Workspace};

use super::{Modal, Placement, Shell, plugins};

/// A command there is now, whoever carries it out: what the palette lists,
/// a key binding names and the command line asks for.
#[derive(Clone, Debug)]
pub(super) struct Listed {
    /// Its stable kebab-case id.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// What the shell sorts its own under.
    pub category: Option<CommandCategory>,
    pub destructive: bool,
    /// Whether it can be carried out now.
    pub enabled: bool,
    by: By,
}

/// Who carries a command out.
#[derive(Clone, Copy, Debug)]
enum By {
    Shell(CommandId),
    /// The plugin the cursor's pane shows.
    Pane,
}

impl Shell {
    /// Every command there is now, in the order the palette lists them:
    /// those of the plugin the cursor's pane shows, then the shell's own.
    pub(super) fn commands(&self) -> Vec<Listed> {
        let pane = self.model.with_untracked(Workspace::cursor_pane_id);
        let shown = pane.and_then(|pane| {
            let plugin = self.plugin(pane)?;
            let shown = self.kept().panes.borrow().get(&pane)?.plugin.clone()?;
            Some((plugin, shown))
        });
        let of_pane = shown.into_iter().flat_map(|(plugin, shown)| {
            (plugins::commands().into_iter())
                .filter(move |listed| listed.plugin == plugin)
                .map(move |listed| Listed {
                    name: listed.name,
                    title: listed.title,
                    description: listed.description,
                    category: None,
                    destructive: listed.destructive,
                    enabled: shown.enabled(listed.name),
                    by: By::Pane,
                })
        });
        let own = (command_entries(self.command_state()).into_iter()).map(|entry| Listed {
            name: entry.spec.name,
            title: entry.spec.title,
            description: entry.spec.description,
            category: Some(entry.spec.category),
            destructive: entry.spec.destructive,
            enabled: entry.enabled,
            by: By::Shell(entry.spec.id),
        });
        of_pane.chain(own).collect()
    }

    /// The command `name` names now. One of the shell's is found before
    /// one of the same name the cursor's pane has.
    pub(super) fn command(&self, name: &str) -> Option<Listed> {
        let mut named: Vec<Listed> = (self.commands().into_iter())
            .filter(|command| command.name == name)
            .collect();
        let own = named
            .iter()
            .position(|command| matches!(command.by, By::Shell(_)));
        (!named.is_empty()).then(|| named.swap_remove(own.unwrap_or(0)))
    }

    /// Carries a command out; `false` where whoever would is gone.
    pub(super) fn run(&self, command: &Listed) -> bool {
        match command.by {
            By::Shell(id) => {
                self.execute(id);
                true
            }
            By::Pane => self.execute_plugin(command.name),
        }
    }

    /// Says on stderr which of the configuration's bindings name no
    /// command there can be: one of the shell's, or of any plugin's.
    pub(super) fn say_unknown_bindings(&self) {
        let mut known: Vec<&str> = CommandId::ALL.iter().map(|id| id.spec().name).collect();
        known.extend(plugins::commands().iter().map(|listed| listed.name));
        for name in self.kept().keymap.borrow().unknown(&known) {
            eprintln!("horizon config: the keybinding to `{name}` names no command");
        }
    }

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
            CommandId::TerminateActiveSession => {
                if let Some(session) = self.model.with_untracked(Workspace::cursor_session_id) {
                    self.terminate_session(session);
                }
                self.change(Workspace::exit_workspace_mode);
            }
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
