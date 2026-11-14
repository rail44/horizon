//! The plugins a pane can show: the commands each carries out, and a
//! pane's view of one made on the state the model keeps for the pane.

use std::rc::Rc;

use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set, Update, WithUntracked};
use horizon_ui::{Callable, Callback, El};
use std::path::{Path, PathBuf};

use horizon_workspace::{PaneId, PluginPane, SessionId, Workspace};

use super::Shell;
use crate::background::Background;
use crate::board::plugin::{
    BoardListPlugin, BoardThreadPlugin, ListAsked, ThreadAsked, ThreadState,
};
use crate::board::{list, thread};
use crate::markdown::{DocumentState, MarkdownPlugin};
use crate::plugin::{Mounted, Plugin};
use crate::preview::{PreviewPlugin, Target};
use crate::theme_settings::{self, ThemeSettingsPlugin};
use crate::wiki::WikiPlugin;
use horizon_theme::seed::Seed;
use horizon_theme_css::Theme;

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
    commands.extend(listed::<PreviewPlugin>());
    commands.extend(listed::<ThemeSettingsPlugin>());
    commands.extend(listed::<MarkdownPlugin>());
    commands.extend(listed::<WikiPlugin>());
    commands
}

/// A pane of `P` on `state`, as the model keeps it.
pub(super) fn pane<P: Plugin>(state: P::State) -> PluginPane {
    PluginPane {
        name: P::NAME.to_string(),
        title: P::title(&state),
        state: serde_json::to_value(state).unwrap_or_default(),
    }
}

/// The board's two plugins, by name.
pub(super) const LIST: &str = BoardListPlugin::NAME;
pub(super) const THREAD: &str = BoardThreadPlugin::NAME;
pub(super) const PREVIEW: &str = PreviewPlugin::NAME;

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
            PreviewPlugin::NAME => {
                let plugin = PreviewPlugin {
                    background: background(),
                    wake: kept.wake.clone(),
                };
                self.mount(pane, plugin, state, |never| match never {})
            }
            ThemeSettingsPlugin::NAME => {
                let config = kept.config.borrow().clone();
                let scheme = self.theme.with_untracked(|theme| *theme.scheme());
                let fallback = theme_settings::fallback(&scheme);
                let seed = Seed::from_current_config(&config, fallback);
                let save = kept.save_theme.clone();
                let host = theme_settings::Host {
                    // The seed's theme, at the font size in use.
                    apply: Box::new(move |seed| {
                        let kept = shell.kept();
                        let mut config = kept.config.borrow().clone();
                        config.theme = seed.to_raw_config().theme;
                        let size = shell.theme.with_untracked(|theme| theme.font_size);
                        let mut theme = Theme::from_config(&config);
                        theme.font_size = size;
                        *kept.config.borrow_mut() = config;
                        shell.theme.set(theme);
                    }),
                    save: Box::new(move |seed| match &save {
                        Some(save) => save(seed),
                        None => horizon_theme::save::save(seed).map_err(|error| error.to_string()),
                    }),
                    path: horizon_config::resolved_path(),
                };
                let plugin = ThemeSettingsPlugin {
                    seed,
                    theme: self.theme,
                    host,
                };
                self.mount(pane, plugin, state, |never| match never {})
            }
            MarkdownPlugin::NAME => {
                let plugin = MarkdownPlugin {
                    open_url: kept.open_url.clone(),
                    wake: kept.wake.clone(),
                };
                self.mount(pane, plugin, state, |never| match never {})
            }
            WikiPlugin::NAME => {
                let plugin = WikiPlugin {
                    open_url: kept.open_url.clone(),
                    wake: kept.wake.clone(),
                };
                self.mount(pane, plugin, state, |never| match never {})
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
            let kept = self::pane::<P>(state.get());
            let changed = shell.model.with_untracked(|model| {
                (model.pane_plugin(pane)).is_some_and(|plugin| *plugin != kept)
            });
            if changed {
                shell
                    .model
                    .update(|model| _ = model.set_pane_plugin(pane, kept));
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

/// A preview pane on `target`.
pub(super) fn preview_of(target: Option<Target>) -> PluginPane {
    pane::<PreviewPlugin>(target)
}

/// What a preview pane shows, as the model keeps it.
pub(super) fn previewed(plugin: &PluginPane) -> Option<Option<Target>> {
    (plugin.name == PREVIEW).then(|| serde_json::from_value(plugin.state.clone()).ok().flatten())
}

/// A theme settings pane.
pub(super) fn theme_settings() -> PluginPane {
    pane::<ThemeSettingsPlugin>(())
}

/// The two document views, by name.
pub(super) const MARKDOWN: &str = MarkdownPlugin::NAME;
pub(super) const WIKI: &str = WikiPlugin::NAME;

/// A pane of the document view `name` on `path`.
pub(super) fn document(name: &str, path: Option<PathBuf>) -> Option<PluginPane> {
    let state = DocumentState { path };
    match name {
        MARKDOWN => Some(pane::<MarkdownPlugin>(state)),
        WIKI => Some(pane::<WikiPlugin>(state)),
        _ => None,
    }
}

/// The pane of the document view `name` that shows `path`, if any.
pub(super) fn document_pane(model: &Workspace, name: &str, path: &Path) -> Option<PaneId> {
    (model.all_pane_ids().into_iter()).find(|pane| {
        model.pane_plugin(*pane).is_some_and(|plugin| {
            let state = serde_json::from_value::<DocumentState>(plugin.state.clone());
            plugin.name == name && state.is_ok_and(|state| state.path.as_deref() == Some(path))
        })
    })
}

/// A list pane.
pub(super) fn list() -> PluginPane {
    pane::<BoardListPlugin>(())
}

/// A thread pane on `task`.
pub(super) fn thread_on(task: u64) -> PluginPane {
    pane::<BoardThreadPlugin>(ThreadState { task })
}
