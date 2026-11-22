//! The commands this shell carries out, of `horizon-workspace`'s command
//! model: the palette and the mode's keys are bindings to them.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use horizon_ui::reactive_graph::owner::Owner;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{GetUntracked, Update, WithUntracked};
use horizon_workspace::commands::{CommandCategory, CommandId, CommandState, command_entries};
use horizon_workspace::{CloseCursorOutcome, PaneId, Side, Workspace};

use super::{Modal, Placement, Shell, plugins};
use crate::extension::{Extension, Running};
use crate::plugin_host::Asks;
use crate::wake::Wake;

/// Says that an extension's own command was not carried out, or stopped.
fn command_failed(command: &str, error: &str) {
    eprintln!("horizon: the command `{command}`: {error}");
}

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
    /// The extension of this name, for itself.
    Extension(&'static str),
}

/// An extension carrying out a command of its own: the instance made for
/// the command, which lasts as long as what it put over the workspace.
pub(super) struct Commanded {
    id: u64,
    command: &'static str,
    running: Rc<Running>,
    /// What it said if it trapped.
    failed: RwSignal<Option<String>>,
    /// What the instance's work beside the page's is made under.
    owner: Owner,
}

impl Shell {
    /// Every command there is now, in the order the palette lists them:
    /// those of the plugin the cursor's pane shows, then the shell's own,
    /// then each extension's own.
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
        let extensions = plugins::extensions();
        let of_extensions = extensions.iter().flat_map(|extension| {
            extension.commands.iter().map(|command| Listed {
                name: command.name,
                title: command.title,
                description: command.description,
                category: None,
                destructive: command.destructive,
                enabled: true,
                by: By::Extension(extension.name),
            })
        });
        of_pane.chain(own).chain(of_extensions).collect()
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
            By::Extension(extension) => match plugins::extension(extension) {
                Some(extension) => {
                    self.run_extension_command(&extension, command.name);
                    true
                }
                None => false,
            },
        }
    }

    /// Carries out an extension's own command in an instance made for it.
    /// The instance is kept for as long as it has a view over the
    /// workspace ([`Shell::let_go_of_commanded`]).
    fn run_extension_command(&self, extension: &Arc<Extension>, command: &'static str) {
        let extension = extension.clone();
        self.run_for_extension(command, move |asks, wake, failed| {
            extension.run_command(command, asks, wake, failed)
        });
    }

    /// Runs what `start` starts in an instance made for it, as an
    /// extension's command (`command` names it when it fails): the
    /// instance is kept for as long as it has a view over the workspace.
    pub(super) fn run_for_extension(
        &self,
        command: &'static str,
        start: impl FnOnce(Asks, Wake, RwSignal<Option<String>>) -> Result<Running, String>,
    ) {
        let kept = self.kept();
        let id = kept.commanded_count.get() + 1;
        kept.commanded_count.set(id);
        let asks = Asks {
            open_url: Some(kept.open_url.clone()),
            set_title: None,
            theme: Some(self.theme_asks()),
            sessions: Some(self.session_asks()),
            modal: Some(self.modal_asks(Some(id))),
            workspace: Some(self.workspace_asks()),
        };
        let owner = Owner::new();
        let failed = owner.with(|| RwSignal::new(None::<String>));
        match start(asks, kept.wake.clone(), failed) {
            Ok(running) => {
                let running = Rc::new(running);
                owner.with(|| running.follow());
                kept.commanded.borrow_mut().push(Commanded {
                    id,
                    command,
                    running,
                    failed,
                    owner,
                });
            }
            Err(error) => {
                owner.cleanup();
                command_failed(command, &error);
            }
        }
    }

    /// Lets go of the instances made for commands that have nothing over
    /// the workspace any more, and of those that stopped: what one of
    /// those had there is taken away, and what it said is said.
    pub(super) fn let_go_of_commanded(&self) {
        let kept = self.kept();
        let shown = (kept.over.borrow().as_ref()).and_then(|over| over.by);
        let (stay, go): (Vec<Commanded>, Vec<Commanded>) = (kept.commanded.take().into_iter())
            .partition(|commanded| shown == Some(commanded.id) && !commanded.running.stopped());
        // (One begun while these go is kept.)
        kept.commanded.borrow_mut().splice(0..0, stay);
        for gone in go {
            if let Some(error) = gone.failed.try_get_untracked().flatten() {
                command_failed(gone.command, &error);
            }
            gone.owner.cleanup();
            // (What it had over the workspace goes with it.)
            drop(gone.running);
        }
    }

    /// How many instances made for commands there are: those with a view
    /// over the workspace.
    pub fn commanded(&self) -> usize {
        self.kept().commanded.borrow().len()
    }

    /// Says on stderr which of the configuration's bindings name no
    /// command there can be: one of the shell's, or of any plugin's.
    pub(super) fn say_unknown_bindings(&self) {
        let mut known: Vec<&str> = CommandId::ALL.iter().map(|id| id.spec().name).collect();
        known.extend(plugins::commands().iter().map(|listed| listed.name));
        let extensions = plugins::extensions();
        known.extend(
            extensions
                .iter()
                .flat_map(|extension| &extension.commands)
                .map(|command| command.name),
        );
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
            CommandId::SplitRight => self.place(Placement::Split(Side::Right)),
            CommandId::SplitDown => self.place(Placement::Split(Side::Down)),
            CommandId::SplitLeft => self.place(Placement::Split(Side::Left)),
            CommandId::SplitUp => self.place(Placement::Split(Side::Up)),
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
            // A file that cannot be read leaves what is in use as it is.
            CommandId::ApplyLayout => self.open(Modal::Layouts),
            CommandId::ResetExtensions => _ = self.reset_extensions(None, |_| {}),
            CommandId::MoveToTab => self.open(Modal::MoveTo),
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

    /// [`close_pane`](Self::close_pane) on `pane`: the cursor is brought
    /// to it first, out of workspace mode if it is on.
    pub(super) fn close_pane_of(&self, pane: PaneId) {
        self.model.update(|model| {
            model.exit_workspace_mode();
            model.activate_pane(pane);
        });
        self.close_pane();
    }
}
