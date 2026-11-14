//! The plugins a pane can show: the commands each carries out, and a
//! pane's view of one made on the state the model keeps for the pane.

use std::rc::Rc;

use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set, Update, WithUntracked};
use horizon_ui::{Callable, Callback, El};
use horizon_workspace::{PaneId, PluginPane, SessionId};

use super::Shell;
use crate::background::Background;
use crate::board::plugin::{
    BoardListPlugin, BoardThreadPlugin, ListAsked, ThreadAsked, ThreadState,
};
use crate::board::{list, thread};
use crate::plugin::{Mounted, Plugin};

/// A plugin's command, as the palette lists it and a key binding names
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub plugin: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub destructive: bool,
}

fn listed<P: Plugin>() -> Vec<Listed> {
    (P::commands().into_iter())
        .map(|command| Listed {
            plugin: P::NAME,
            name: command.name,
            title: command.title,
            description: command.description,
            destructive: command.destructive,
        })
        .collect()
}

/// Every plugin's commands, in the order the palette lists them.
pub(super) fn commands() -> Vec<Listed> {
    let mut commands = listed::<BoardListPlugin>();
    commands.extend(listed::<BoardThreadPlugin>());
    commands
}

/// A pane of `P` on `state`, as the model keeps it.
pub(super) fn pane<P: Plugin>(state: P::State) -> PluginPane {
    PluginPane {
        name: P::NAME.to_string(),
        title: P::TITLE.to_string(),
        state: serde_json::to_value(state).unwrap_or_default(),
    }
}

/// The board's two plugins, by name.
pub(super) const LIST: &str = BoardListPlugin::NAME;
pub(super) const THREAD: &str = BoardThreadPlugin::NAME;

/// What a pane keeps of the plugin it shows.
#[derive(Clone)]
pub(super) struct Shown {
    pub plugin: &'static str,
    run: Rc<dyn Fn(&str) -> bool>,
}

impl Shown {
    /// Carries out the command named `name`; `false` when the plugin has
    /// none of that name.
    pub fn run(&self, name: &str) -> bool {
        (self.run)(name)
    }
}

impl Shell {
    /// The view of the plugin `shown` names, on its state: `None` when no
    /// plugin has that name or the state is not one it keeps.
    pub(super) fn mount_plugin(
        &self,
        pane: PaneId,
        shown: &PluginPane,
        bound: RwSignal<Vec<SessionId>>,
    ) -> Option<(El, Shown)> {
        let shell = *self;
        let kept = self.kept();
        let background = || Background::new(kept.wake.clone());
        let activity = kept.board_activity.into();
        let state = &shown.state;
        match shown.name.as_str() {
            BoardListPlugin::NAME => {
                let source = self.board_source();
                // (The board's project, for what the view asks that is
                // the project's.)
                let project = source.root();
                let plugin = BoardListPlugin {
                    source,
                    background: background(),
                    activity,
                };
                self.mount(pane, plugin, state, move |asked| match asked {
                    ListAsked::View(list::Asked::OpenThread(task)) => {
                        shell.open_thread(pane, task);
                    }
                    ListAsked::View(list::Asked::OpenOrganizer { failed }) => {
                        shell.open_board_organizer(pane, project.clone(), failed);
                    }
                    ListAsked::Bound(sessions) => bound.set(sessions),
                })
            }
            BoardThreadPlugin::NAME => {
                let source = self.board_source();
                let project = source.root();
                let plugin = BoardThreadPlugin {
                    source,
                    background: background(),
                    activity,
                };
                self.mount(pane, plugin, state, move |asked| match asked {
                    ThreadAsked::View(thread::Asked::OpenSession { session, failed }) => {
                        shell.open_task_session(session, failed);
                    }
                    ThreadAsked::View(thread::Asked::OpenOrganizer { failed }) => {
                        shell.open_board_organizer(pane, project.clone(), failed);
                    }
                    ThreadAsked::Bound(sessions) => bound.set(sessions),
                    ThreadAsked::BackToList => shell.back_to_board_list(pane),
                })
            }
            _ => None,
        }
    }

    /// `plugin`'s view on `state`, whose state the model keeps for the
    /// pane as it changes.
    fn mount<P: Plugin>(
        &self,
        pane: PaneId,
        plugin: P,
        state: &serde_json::Value,
        ask: impl Fn(P::Asked) + 'static,
    ) -> Option<(El, Shown)> {
        let shell = *self;
        let state: P::State = serde_json::from_value(state.clone()).ok()?;
        let Mounted { view, run, state } = plugin.mount(state, Callback::new(ask));
        Effect::new(move |_| {
            let Ok(state) = serde_json::to_value(state.get()) else {
                return;
            };
            let changed = shell.model.with_untracked(|model| {
                model
                    .pane_plugin(pane)
                    .is_some_and(|plugin| plugin.state != state)
            });
            if changed {
                shell
                    .model
                    .update(|model| _ = model.set_pane_plugin_state(pane, state));
            }
        });
        let commands = P::commands();
        let run = move |name: &str| {
            let command = commands.iter().find(|command| command.name == name);
            command.map(|command| run.run(command.command)).is_some()
        };
        Some((
            view,
            Shown {
                plugin: P::NAME,
                run: Rc::new(run),
            },
        ))
    }
}

/// A list pane.
pub(super) fn list() -> PluginPane {
    pane::<BoardListPlugin>(())
}

/// A thread pane on `task`.
pub(super) fn thread_on(task: u64) -> PluginPane {
    pane::<BoardThreadPlugin>(ThreadState { task })
}
