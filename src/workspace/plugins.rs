//! The plugins a pane can show: the commands each carries out, and a
//! pane's view of one made on the state the model keeps for the pane.

use std::rc::Rc;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::GetUntracked;
use horizon_ui::reactive_graph::traits::{Get, Set, Update, WithUntracked};
use horizon_ui::{Callable, Callback, El, el};

mod styles {
    horizon_ui::css_module! {"
    .extension { flex: 1; min-height: 0; display: flex; flex-direction: column;
                 background: var(--background); color: var(--text); }
    .extension > * { flex: 1; min-height: 0; }
    .extension > .failed { flex: none; padding: var(--u16); color: var(--danger);
                           font-size: var(--meta-size); line-height: var(--meta-line);
                           white-space: pre-wrap; }
"}
}
use std::path::{Path, PathBuf};

use horizon_workspace::{PaneId, PluginPane, SessionId, Workspace};

use super::Shell;
use crate::agent::plugin::AgentPlugin;
use crate::background::Background;
use crate::board::plugin::{
    BoardListPlugin, BoardThreadPlugin, ListAsked, ThreadAsked, ThreadState,
};
use crate::board::{list, thread};
use crate::extension::Extension;
use crate::markdown::{DocumentState, MarkdownPlugin};
use crate::plugin::{Mounted, Plugin};
use crate::plugin_host::Asks;
use crate::preview::{PreviewPlugin, Target};
use crate::terminal::plugin::TerminalPlugin;
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

thread_local! {
    /// The extensions this shell loaded (`config.toml`'s `extensions`).
    static EXTENSIONS: std::cell::RefCell<Vec<Rc<Extension>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Takes in the extensions a pane can show.
pub(super) fn take_in(extensions: Vec<Rc<Extension>>) {
    EXTENSIONS.with(|kept| *kept.borrow_mut() = extensions);
}

/// The extension named `name`, if one was loaded.
fn extension(name: &str) -> Option<Rc<Extension>> {
    EXTENSIONS.with(|kept| {
        kept.borrow()
            .iter()
            .find(|extension| extension.name == name)
            .cloned()
    })
}

/// Every plugin's commands, in the order the palette lists them.
pub(super) fn commands() -> Vec<Listed> {
    let mut commands = listed::<BoardListPlugin>();
    commands.extend(listed::<BoardThreadPlugin>());
    commands.extend(listed::<PreviewPlugin>());
    commands.extend(listed::<ThemeSettingsPlugin>());
    commands.extend(listed::<MarkdownPlugin>());
    commands.extend(listed::<WikiPlugin>());
    commands.extend(listed::<TerminalPlugin>());
    commands.extend(listed::<AgentPlugin>());
    EXTENSIONS.with(|kept| {
        for extension in kept.borrow().iter() {
            commands.extend(extension.commands.iter().map(|command| Listed {
                plugin: extension.name,
                name: command.name,
                title: command.title,
                description: command.description,
                destructive: command.destructive,
            }));
        }
    });
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
    enabled: Rc<dyn Fn(&str) -> bool>,
}

impl Shown {
    /// What a pane keeps of a plugin that could not be shown.
    fn none(plugin: &'static str) -> Self {
        Self {
            plugin,
            run: Rc::new(|_: &str| false),
            enabled: Rc::new(|_: &str| false),
        }
    }

    /// Carries out the command named `name`; `false` when the plugin has
    /// none of that name.
    pub fn run(&self, name: &str) -> bool {
        (self.run)(name)
    }

    /// Whether the command named `name` can be carried out now.
    pub fn enabled(&self, name: &str) -> bool {
        (self.enabled)(name)
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
            name => {
                let extension = extension(name)?;
                Some(self.mount_extension(pane, extension, shown))
            }
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
        let state: P::State = serde_json::from_value(state.clone()).ok()?;
        Some(self.shown::<P>(pane, plugin.mount(state, Callback::new(ask))))
    }

    /// What a pane keeps of `P`'s view: the state it keeps goes to the
    /// model as it changes.
    pub(super) fn shown<P: Plugin>(
        &self,
        pane: PaneId,
        mounted: Mounted<P::State, P::Command>,
    ) -> (El, Shown) {
        let shell = *self;
        let Mounted {
            view,
            run,
            enabled,
            state,
        } = mounted;
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
        let commands = Rc::new(P::commands());
        let named = commands.clone();
        let run = move |name: &str| {
            let command = named.iter().find(|command| command.name == name);
            command.map(|command| run.run(command.command)).is_some()
        };
        let enabled = move |name: &str| {
            let Some(command) = commands.iter().find(|command| command.name == name) else {
                return false;
            };
            enabled
                .as_ref()
                .is_none_or(|enabled| enabled.run(command.command))
        };
        (
            view,
            Shown {
                plugin: P::NAME,
                run: Rc::new(run),
                enabled: Rc::new(enabled),
            },
        )
    }
}

impl Shell {
    /// A pane of `extension` on the state the model keeps for it, which
    /// goes back to the model as it changes, with the pane's title.
    fn mount_extension(
        &self,
        pane: PaneId,
        extension: Rc<Extension>,
        shown: &PluginPane,
    ) -> (El, Shown) {
        let shell = *self;
        let kept = self.kept();
        let failed = RwSignal::new(None::<String>);
        let title = RwSignal::new(shown.title.clone());
        let root = el("div").class(styles::EXTENSION).when(
            move || failed.get().is_some(),
            move || {
                el("div")
                    .class(styles::FAILED)
                    .text_dyn(move || failed.get().unwrap_or_default())
            },
        );
        let asks = Asks {
            open_url: Some(kept.open_url.clone()),
            set_title: Some(Rc::new(move |text| title.set(text))),
        };
        let name = extension.name;
        let running = match extension.instantiate(asks, failed) {
            Ok(running) => Rc::new(running),
            Err(error) => {
                failed.set(Some(error));
                return (root, Shown::none(name));
            }
        };
        match running.mount(shown.state.as_str()) {
            Ok(node) => {
                root.child(El(NodeId::from_u64(node)));
            }
            Err(error) => failed.update(|failed| {
                failed.get_or_insert(error);
            }),
        }
        running.follow();
        // What the pane keeps, and its title, after the page's effects.
        let keeping = running.clone();
        horizon_ui::each_effects(move || {
            let state = keeping.state().map(serde_json::Value::String);
            let now = PluginPane {
                name: name.to_string(),
                title: title.get_untracked(),
                state: state.unwrap_or_default(),
            };
            let changed = (shell.model)
                .with_untracked(|model| model.pane_plugin(pane).is_some_and(|kept| *kept != now));
            if changed {
                shell
                    .model
                    .update(|model| _ = model.set_pane_plugin(pane, now));
            }
        });
        let shown = Shown {
            plugin: name,
            run: Rc::new(move |command: &str| running.run(command)),
            enabled: Rc::new(|_: &str| true),
        };
        (root, shown)
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
