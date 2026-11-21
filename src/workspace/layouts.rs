//! The layouts of the configuration (`[layouts.<name>]`,
//! `docs/workspace-layout-design.md`): the list to choose one from, and
//! putting the workspace in one -- by the `Apply Layout…` command, and in
//! `startup_layout` once the extensions are read.

use horizon_config::{Layout, LayoutAxis};
use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Update};
use horizon_workspace::{Arrangement, SplitAxis};

use super::picker::{self, Row};
use super::{Shell, styles};
use crate::agent::plugin::AgentPlugin;
use crate::plugin::{Given, Plugin};
use crate::terminal::plugin::TerminalPlugin;

impl Shell {
    /// The names of the configuration's layouts, in order.
    fn layout_names(&self) -> Vec<String> {
        self.kept()
            .config
            .borrow()
            .layouts
            .keys()
            .cloned()
            .collect()
    }

    /// Puts the workspace in the configuration's layout `name`. What is
    /// not done is said on stderr, as a command that fails is.
    pub fn apply_layout(&self, name: &str) {
        let layout = self.kept().config.borrow().layout(name);
        let arrangement = layout.and_then(|layout| self.arrangement_of(&layout));
        match arrangement {
            // The model only: the panes are made, and the keys given, where
            // the workspace is arranged on the page next -- this may be
            // asked while the page is laid out, as the extensions arrive.
            Ok(arrangement) => {
                self.model.update(|model| _ = model.arrange(&arrangement));
                self.kept().refocus.set(true);
            }
            Err(error) => eprintln!("horizon: {error}"),
        }
    }

    /// Puts the workspace in `startup_layout`, the first time the
    /// extensions are read: a layout's views may be theirs.
    pub(super) fn apply_startup_layout(&self) {
        let kept = self.kept();
        if kept.startup_layout_applied.replace(true) {
            return;
        }
        let name = kept.config.borrow().startup_layout.clone();
        if let Some(name) = name {
            self.apply_layout(&name);
        }
    }

    /// `layout` with its views made, each as `horizon new <name>
    /// <arguments>` makes it, from where the cursor's session works.
    fn arrangement_of(&self, layout: &Layout) -> Result<Arrangement, String> {
        Ok(match layout {
            Layout::View { name, arguments } => {
                if name == TerminalPlugin::NAME || name == AgentPlugin::NAME {
                    return Err(format!(
                        "a layout's views hold no session: `{name}` is not one"
                    ));
                }
                let given = Given {
                    arguments: arguments.clone(),
                    directory: self.working_directory(),
                    ..Given::default()
                };
                let made = match super::plugins::created(name, given.clone()) {
                    Some(made) => made?,
                    None => self.created_by_extension(name, given)?,
                };
                Arrangement::View(made)
            }
            Layout::Workspace => Arrangement::Workspace,
            Layout::Split { axis, children } => Arrangement::Split {
                axis: match axis {
                    LayoutAxis::Horizontal => SplitAxis::Horizontal,
                    LayoutAxis::Vertical => SplitAxis::Vertical,
                },
                children: (children.iter())
                    .map(|(weight, child)| Ok((*weight, self.arrangement_of(child)?)))
                    .collect::<Result<_, String>>()?,
            },
            Layout::Tabs(tabs) => Arrangement::Tabs(
                (tabs.iter())
                    .map(|tab| self.arrangement_of(tab))
                    .collect::<Result<_, String>>()?,
            ),
        })
    }
}

/// The list of the configuration's layouts, one chosen to put the
/// workspace in.
pub(super) fn view(shell: Shell) -> El {
    let matching = move |query: &str| {
        let query = query.trim().to_ascii_lowercase();
        (shell.layout_names().into_iter())
            .filter(|name| query.is_empty() || name.to_ascii_lowercase().contains(&query))
            .collect::<Vec<_>>()
    };
    let rows = move |query: &str| {
        (matching(query).into_iter())
            .map(|name| Row {
                key: name.clone(),
                class: (styles::COMMAND, None),
                parts: vec![(styles::NAME, name)],
                indent: 0.,
            })
            .collect()
    };
    let picked = move |index: usize| {
        let names = matching(&shell.search.query.get_untracked());
        if let Some(name) = names.get(index) {
            shell.modal.set(None);
            shell.apply_layout(name);
        }
    };
    picker::view(shell.search, rows, picked, "apply")
}
