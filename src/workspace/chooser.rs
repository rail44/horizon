//! The view chooser: which view a new pane shows. A terminal, an agent,
//! an agent in one of the roles a person can start one in, the board,
//! the wiki, or an extension (the Markdown view and the theme settings
//! are extensions the shell comes with); an extension that
//! makes its pane on a file has the file found next.

use horizon_agent::roles::{RoleId, user_launchable};
use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_workspace::{PluginPane, SessionKind};

use super::picker::{self, Picked, Row};
use super::{Modal, Placement, Shell, plugins, styles};
use crate::extension::Extension;
use std::sync::Arc;

/// What a new pane shows.
enum Choice {
    Session(SessionKind, Option<RoleId>),
    Plugin(PluginPane),
    /// The wiki of the project the shell was started in.
    Wiki,
    Extension(Arc<Extension>),
}

/// The views to choose from whose titles hold `query`.
fn matching(query: &str) -> Vec<(String, Choice)> {
    let mut choices = vec![
        (
            "Terminal".to_string(),
            Choice::Session(SessionKind::Terminal, None),
        ),
        (
            "Agent".to_string(),
            Choice::Session(SessionKind::Agent, None),
        ),
    ];
    for role in user_launchable() {
        let id = RoleId(role.id.to_string());
        let choice = Choice::Session(SessionKind::Agent, Some(id));
        choices.push((role.title.to_string(), choice));
    }
    choices.push(("Board".to_string(), Choice::Plugin(plugins::list())));
    choices.push(("Wiki".to_string(), Choice::Wiki));
    for extension in plugins::extensions() {
        choices.push((extension.title.clone(), Choice::Extension(extension)));
    }
    let query = query.trim().to_ascii_lowercase();
    choices.retain(|(title, ..)| query.is_empty() || title.to_ascii_lowercase().contains(&query));
    choices
}

pub(super) fn view(shell: Shell, placement: Placement) -> El {
    let rows = |query: &str| {
        (matching(query).into_iter())
            .map(|(title, ..)| Row {
                key: title.clone(),
                class: (styles::COMMAND, None),
                parts: vec![(styles::NAME, title)],
                indent: 0.,
            })
            .collect()
    };
    let picked = move |index: usize, picked: Picked| {
        if !matches!(picked, Picked::Confirmed) {
            return;
        }
        let mut choices = matching(&shell.query.get_untracked());
        if index < choices.len() {
            let (_, choice) = choices.swap_remove(index);
            shell.modal.set(None);
            match choice {
                Choice::Session(kind, role) => shell.create(kind, role, placement),
                Choice::Wiki => shell.create_wiki(placement),
                Choice::Plugin(plugin) => shell.create_plugin(plugin, placement),
                Choice::Extension(extension) if extension.file_asked_for().is_some() => {
                    shell.open(Modal::Finder(extension.name, placement));
                }
                // (Made from nothing a person is asked for: where the
                // pane was asked for, and what may be left out.)
                Choice::Extension(extension) => {
                    match extension.created_on(None, &shell.working_directory()) {
                        Ok(state) => shell.create_plugin(
                            PluginPane {
                                name: extension.name.to_string(),
                                title: extension.title.clone(),
                                state: state
                                    .map_or(serde_json::Value::Null, serde_json::Value::String),
                            },
                            placement,
                        ),
                        Err(error) => {
                            eprintln!("horizon: {}: {error}", extension.name);
                            shell.focus_active();
                        }
                    }
                }
            }
        }
    };
    picker::view(shell, rows, picked)
}
