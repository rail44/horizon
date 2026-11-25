//! The model picker: an agent session's provider, then one of its models,
//! as the session's `model` option offers them. Above the providers, the
//! session's reasoning effort, when its model takes one: a step of its own
//! listing the levels the model takes.

use horizon_ui::El;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, WithUntracked};
use horizon_workspace::{SessionId, Workspace};

use super::picker::{self, Row};
use super::{Models, Shell, styles};

/// Which list the picker shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// The effort row, then the providers.
    Providers,
    /// The models of the provider at this place.
    Models(usize),
    /// The levels the session's model takes.
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
    let matching_levels = move |query: &str| -> Vec<String> {
        let choices =
            held.with_untracked(|effort| (effort.as_ref()).map(|effort| effort.choices.clone()));
        (choices.unwrap_or_default().into_iter())
            .filter(|level| holds(level, query))
            .collect()
    };
    // The providers that match, by their places among all of them.
    let providers = choices.providers.clone();
    let matching_providers = move |query: &str| -> Vec<(usize, String)> {
        (providers.iter().enumerate())
            .filter(|(_, (name, _))| holds(name, query))
            .map(|(place, (name, _))| (place, name.clone()))
            .collect()
    };
    let providers = choices.providers.clone();
    let matching_models = move |provider: usize, query: &str| -> Vec<String> {
        (providers.get(provider).into_iter())
            .flat_map(|(_, models)| models.iter())
            .filter(|model| holds(model, query))
            .cloned()
            .collect()
    };
    let current = choices.current.clone();
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
                    (effort.as_ref()).map_or(String::new(), |effort| effort.current.clone()),
                ),
            ],
            indent: 0.,
        }
    };
    let current_level = effort.map(|effort| effort.current);
    let rows = move |query: &str| match stage.get() {
        Stage::Effort => {
            let rows: Vec<Row> = (matching_levels(query).into_iter())
                .map(|level| {
                    let about = match current_level.as_deref() == Some(level.as_str()) {
                        true => "current",
                        false => "",
                    };
                    Row {
                        key: level.clone(),
                        class: (styles::COMMAND, None),
                        parts: vec![(styles::NAME, level), (styles::ABOUT, about.to_string())],
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
                    key: model.clone(),
                    class: (styles::COMMAND, None),
                    parts: vec![(styles::NAME, model)],
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
                shell.modal.set(None);
                shell.change(Workspace::exit_workspace_mode);
                // The session says so itself when its effort has changed.
                let source = models.clone();
                shell.in_background(
                    move || source.set_effort(session, &level),
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
                let Some((name, _)) = choices.providers.get(provider).cloned() else {
                    return;
                };
                shell.modal.set(None);
                shell.change(Workspace::exit_workspace_mode);
                // The session says so itself when its model has changed.
                let source = models.clone();
                shell.in_background(
                    move || source.switch(session, &name, &model),
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
