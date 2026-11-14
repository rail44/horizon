//! The model picker: an agent session's provider, then one of the models
//! that provider lists. Both are asked of the agent runtime when the list
//! needs them, off the UI thread.

use std::collections::HashMap;

use horizon_acp::ProviderSummary;
use horizon_ui::El;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_workspace::{SessionId, Workspace};

use super::picker::{self, Picked, Row};
use super::{Models, Shell, styles};

/// A provider's models: `None` while they are being asked for.
type Listed = HashMap<usize, Option<Vec<String>>>;

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

/// Why a provider cannot be chosen.
fn unavailable_reason(provider: &ProviderSummary) -> String {
    match provider.api_key_env.is_empty() {
        true => "not configured".to_string(),
        false => format!("environment variable `{}` is not set", provider.api_key_env),
    }
}

pub(super) fn view(shell: Shell, session: SessionId, models: std::sync::Arc<dyn Models>) -> El {
    // `None` while the providers are being asked for.
    let providers = RwSignal::new(None::<Vec<ProviderSummary>>);
    // The provider chosen, once one is.
    let stage = RwSignal::new(None::<usize>);
    let listed = RwSignal::new(Listed::new());
    let source = models.clone();
    shell.in_background(
        move || source.providers(),
        move |_, answer| match answer {
            Ok(answer) => providers.set(Some(answer)),
            Err(error) => {
                eprintln!("model picker: {error}");
                providers.set(Some(Vec::new()));
            }
        },
    );

    // The providers that match, by their places among all of them.
    let matching_providers = move |query: &str| -> Vec<(usize, ProviderSummary)> {
        (providers.get_untracked().unwrap_or_default().into_iter())
            .enumerate()
            .filter(|(_, provider)| holds(&provider.name, query))
            .collect()
    };
    // Untracked, as `matching_providers` is: `rows` has already read
    // `listed` for the provider when it gets here.
    let matching_models = move |provider: usize, query: &str| -> Vec<String> {
        let all = listed.with_untracked(|listed| listed.get(&provider).cloned().flatten());
        (all.unwrap_or_default().into_iter())
            .filter(|model| holds(model, query))
            .collect()
    };
    let rows = move |query: &str| match stage.get() {
        None => {
            if providers.with(Option::is_none) {
                return saying("Loading providers…");
            }
            let rows: Vec<Row> = (matching_providers(query).into_iter())
                .map(|(_, provider)| {
                    let about = match (provider.available, &provider.default_model) {
                        (false, _) => unavailable_reason(&provider),
                        (true, Some(model)) => model.clone(),
                        (true, None) => String::new(),
                    };
                    Row {
                        key: format!("{} {about}", provider.name),
                        class: (
                            styles::COMMAND,
                            (!provider.available).then_some(styles::DISABLED),
                        ),
                        parts: vec![(styles::NAME, provider.name), (styles::ABOUT, about)],
                        indent: 0.,
                    }
                })
                .collect();
            match rows.is_empty() {
                true => saying("No providers configured"),
                false => rows,
            }
        }
        Some(provider) => {
            let state = listed.with(|listed| listed.get(&provider).cloned());
            match state {
                None | Some(None) => return saying("Loading models…"),
                Some(Some(all)) if all.is_empty() => return saying("No models listed"),
                Some(Some(_)) => {}
            }
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
    let picked = move |index: usize, picked: Picked| {
        if !matches!(picked, Picked::Confirmed) {
            return;
        }
        let query = shell.query.get_untracked();
        match stage.get_untracked() {
            None => {
                let Some((provider, summary)) = matching_providers(&query).into_iter().nth(index)
                else {
                    return;
                };
                if !summary.available {
                    return;
                }
                // Its models are asked for once.
                let asked = listed.with_untracked(|listed| listed.contains_key(&provider));
                if !asked {
                    listed.update(|listed| {
                        listed.insert(provider, None);
                    });
                    let source = models.clone();
                    shell.in_background(
                        move || source.models(&summary.name),
                        move |_, answer| {
                            let models = answer.unwrap_or_else(|error| {
                                eprintln!("model picker: {error}");
                                Vec::new()
                            });
                            listed.update(|listed| {
                                listed.insert(provider, Some(models));
                            });
                        },
                    );
                }
                stage.set(Some(provider));
                shell.query.set(String::new());
                shell.chosen.set(0);
            }
            Some(provider) => {
                let Some(model) = matching_models(provider, &query).into_iter().nth(index) else {
                    return;
                };
                let name = (providers.get_untracked().unwrap_or_default().get(provider))
                    .map(|provider| provider.name.clone());
                let Some(name) = name else {
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
    // Escape from a provider's models goes back to the providers.
    let back = move || {
        let stepped = stage.get_untracked().is_some();
        if stepped {
            stage.set(None);
            shell.query.set(String::new());
            shell.chosen.set(0);
        }
        stepped
    };
    picker::view_in_steps(shell, rows, picked, back)
}
