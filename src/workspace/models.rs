//! The model picker: a group of an agent session's models, then one of
//! them, as its config option of the `model` category offers them. Above
//! the groups, the session's reasoning effort, when it has an option of the
//! `thought_level` category: a step of its own listing its entries.

use horizon_ui::El;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_workspace::{SessionId, Workspace};

use horizon_shell_runtime::agent_model::Choice;

use super::picker::{self, Row};
use super::{Models, Shell, styles};

/// Which list the picker shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// The effort row, then the groups.
    Providers,
    /// The models of the group at this place.
    Models(usize),
    /// The effort's entries.
    Effort,
}

/// A row that only says something.
fn saying(text: &str) -> Vec<Row> {
    vec![Row {
        key: text.to_string(),
        class: (styles::COMMAND, Some(styles::DISABLED)),
        parts: vec![(styles::NAME, text.to_string())],
        indent: 0.,
    }]
}

fn holds(text: &str, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty() || text.to_ascii_lowercase().contains(&query)
}

/// The picker for `session`, opened at its effort step when `at_effort`.
pub(super) fn view(
    shell: Shell,
    session: SessionId,
    models: std::sync::Arc<dyn Models>,
    at_effort: bool,
) -> El {
    // The session's models and effort as the picker opened on them.
    let agent = shell.kept().agents.borrow().get(&session).cloned();
    let choices = (agent.as_ref())
        .map(|agent| agent.models())
        .unwrap_or_default();
    let effort = agent.and_then(|agent| agent.effort());
    let stage = RwSignal::new(match (at_effort, &effort) {
        (true, Some(_)) => Stage::Effort,
        _ => Stage::Providers,
    });
    let held = RwSignal::new(effort.clone());
    // The effort row is above the providers while it matches.
    let effort_shown =
        move |query: &str| held.with_untracked(Option::is_some) && holds("effort", query);
    let matching_levels = move |query: &str| -> Vec<Choice> {
        let choices =
            held.with_untracked(|effort| (effort.as_ref()).map(|effort| effort.choices.clone()));
        (choices.unwrap_or_default().into_iter())
            .filter(|level| holds(&level.name, query))
            .collect()
    };
    // The groups that match, by their places among all of them.
    let groups = choices.groups.clone();
    let matching_providers = move |query: &str| -> Vec<(usize, String)> {
        (groups.iter().enumerate())
            .filter(|(_, group)| holds(&group.name, query))
            .map(|(place, group)| (place, group.name.clone()))
            .collect()
    };
    let groups = choices.groups.clone();
    let matching_models = move |group: usize, query: &str| -> Vec<Choice> {
        (groups.get(group).into_iter())
            .flat_map(|group| group.entries.iter())
            .filter(|entry| holds(&entry.name, query))
            .cloned()
            .collect()
    };
    let current = (choices.current()).map(|(group, entry)| (group.to_string(), entry.name.clone()));
    let model_option = choices.config_id.clone();
    // The rows and the pick each read them.
    let (pick_providers, pick_models) = (matching_providers.clone(), matching_models.clone());
    let effort_row = {
        let effort = effort.clone();
        move || Row {
            key: "effort".to_string(),
            class: (styles::COMMAND, None),
            parts: vec![
                (styles::NAME, "Effort".to_string()),
                (
                    styles::ABOUT,
                    (effort.as_ref()).map_or(String::new(), |effort| effort.current.name.clone()),
                ),
            ],
            indent: 0.,
        }
    };
    let current_level = effort.as_ref().map(|effort| effort.current.value.clone());
    let effort_option = effort.map(|effort| effort.config_id);
    let rows = move |query: &str| match stage.get() {
        Stage::Effort => {
            let rows: Vec<Row> = (matching_levels(query).into_iter())
                .map(|level| {
                    let about = match current_level.as_deref() == Some(level.value.as_str()) {
                        true => "current",
                        false => "",
                    };
                    Row {
                        key: level.value,
                        class: (styles::COMMAND, None),
                        parts: vec![
                            (styles::NAME, level.name),
                            (styles::ABOUT, about.to_string()),
                        ],
                        indent: 0.,
                    }
                })
                .collect();
            match rows.is_empty() {
                true => saying("No matching levels"),
                false => rows,
            }
        }
        Stage::Providers => {
            let above: Vec<Row> = effort_shown(query).then(&effort_row).into_iter().collect();
            let rows: Vec<Row> = (matching_providers(query).into_iter())
                .map(|(_, provider)| {
                    // The current model is said beside its provider.
                    let about = (current.as_ref())
                        .filter(|(at, _)| *at == provider)
                        .map(|(_, model)| model.clone())
                        .unwrap_or_default();
                    Row {
                        key: format!("{provider} {about}"),
                        class: (styles::COMMAND, None),
                        parts: vec![(styles::NAME, provider), (styles::ABOUT, about)],
                        indent: 0.,
                    }
                })
                .collect();
            let rows = match rows.is_empty() {
                true => saying("No models to switch to"),
                false => rows,
            };
            above.into_iter().chain(rows).collect()
        }
        Stage::Models(provider) => {
            let rows: Vec<Row> = (matching_models(provider, query).into_iter())
                .map(|model| Row {
                    key: model.value,
                    class: (styles::COMMAND, None),
                    parts: vec![(styles::NAME, model.name)],
                    indent: 0.,
                })
                .collect();
            match rows.is_empty() {
                true => saying("No matching models"),
                false => rows,
            }
        }
    };
    let picked = move |index: usize| {
        let query = shell.search.query.get_untracked();
        match stage.get_untracked() {
            Stage::Effort => {
                let Some(level) = matching_levels(&query).into_iter().nth(index) else {
                    return;
                };
                let Some(option) = effort_option.clone() else {
                    return;
                };
                shell.modal.set(None);
                shell.change(Workspace::exit_workspace_mode);
                // The session says so itself when its effort has changed.
                let source = models.clone();
                shell.in_background(
                    move || source.set_option(session, &option, &level.value),
                    |_, answer| {
                        if let Err(error) = answer {
                            eprintln!("effort switch failed: {error}");
                        }
                    },
                );
            }
            Stage::Providers => {
                let above = usize::from(effort_shown(&query));
                if index < above {
                    stage.set(Stage::Effort);
                    shell.search.query.set(String::new());
                    shell.search.chosen.set(0);
                    return;
                }
                let Some((provider, _)) = pick_providers(&query).into_iter().nth(index - above)
                else {
                    return;
                };
                stage.set(Stage::Models(provider));
                shell.search.query.set(String::new());
                shell.search.chosen.set(0);
            }
            Stage::Models(provider) => {
                let Some(model) = pick_models(provider, &query).into_iter().nth(index) else {
                    return;
                };
                let option = model_option.clone();
                shell.modal.set(None);
                shell.change(Workspace::exit_workspace_mode);
                // The session says so itself when its model has changed.
                let source = models.clone();
                shell.in_background(
                    move || source.set_option(session, &option, &model.value),
                    |_, answer| {
                        if let Err(error) = answer {
                            eprintln!("model switch failed: {error}");
                        }
                    },
                );
            }
        }
    };
    // Escape from a provider's models, or from the levels, goes back to the
    // providers.
    let back = move || {
        let stepped = stage.get_untracked() != Stage::Providers;
        if stepped {
            stage.set(Stage::Providers);
            shell.search.query.set(String::new());
            shell.search.chosen.set(0);
        }
        stepped
    };
    picker::view_in_steps(shell.search, rows, picked, "choose", back)
}
