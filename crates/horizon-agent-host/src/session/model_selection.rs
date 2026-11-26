//! Resolve incoming selections once; record only the provider's applied feedback.

use horizon_agent::contract::{Command, ProviderEvent, SessionId};

use super::state::{AgentHostState, lock_unpoisoned};

pub(super) fn resolve(
    state: &AgentHostState,
    provider: &str,
    model: &str,
) -> Result<Command, String> {
    let config = lock_unpoisoned(&state.agent_config);
    horizon_agent::config::resolve_model_selection(&config.providers, provider, model)
        .map(|selection| Command::ApplySessionModel(Box::new(selection)))
}

pub(super) fn record_applied(state: &AgentHostState, session_id: SessionId, event: &ProviderEvent) {
    if !matches!(
        event,
        ProviderEvent::SessionModel(_)
            | ProviderEvent::SessionSelection(_)
            | ProviderEvent::SessionReasoning(_)
    ) {
        return;
    }
    let mut sessions = lock_unpoisoned(&state.sessions);
    if let Some(entry) = sessions.get_mut(&session_id) {
        match event {
            ProviderEvent::SessionModel(model) => entry.model = Some(model.clone()),
            ProviderEvent::SessionSelection(selection) => entry.selection = Some(selection.clone()),
            ProviderEvent::SessionReasoning(reasoning) => entry.reasoning = Some(reasoning.clone()),
            _ => {}
        }
    }
}

/// The model and reasoning effort last chosen on a session's branch
/// (`Event::ModelSelected`, `Event::ReasoningSelected`).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Selections {
    model: Option<(String, String)>,
    reasoning: Option<horizon_agent::config::ReasoningLevel>,
}

pub(super) fn selections_in(history: &[horizon_agent::contract::Event]) -> Selections {
    use horizon_agent::contract::Event;
    let mut selections = Selections::default();
    for event in history {
        match event {
            Event::ModelSelected { provider, model } => {
                selections.model = Some((provider.clone(), model.clone()));
            }
            Event::ReasoningSelected { level } => selections.reasoning = Some(*level),
            _ => {}
        }
    }
    selections
}

/// Switches a session started again over its history back to what was
/// chosen on its branch, without recording the choice a second time. A
/// model the configuration no longer has is said on stderr and left.
pub(super) fn restore(
    state: &AgentHostState,
    session_id: SessionId,
    commands_tx: &crossbeam_channel::Sender<Command>,
    selections: Selections,
) {
    if let Some((provider, model)) = selections.model {
        match resolve(state, &provider, &model) {
            Ok(command) => {
                let _ = commands_tx.send(command);
            }
            Err(message) => eprintln!(
                "horizon-agent: session {session_id:?} keeps its default model: {message}"
            ),
        }
    }
    if let Some(level) = selections.reasoning {
        let _ = commands_tx.send(Command::SetSessionReasoning { level });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::config::ReasoningLevel;
    use horizon_agent::contract::Event;

    #[test]
    fn the_last_choice_of_each_on_the_branch_is_restored() {
        let selected = |model: &str| Event::ModelSelected {
            provider: "default".into(),
            model: model.into(),
        };
        let history = [
            selected("first"),
            Event::ReasoningSelected {
                level: ReasoningLevel::Low,
            },
            selected("second"),
        ];
        assert_eq!(
            selections_in(&history),
            Selections {
                model: Some(("default".into(), "second".into())),
                reasoning: Some(ReasoningLevel::Low),
            }
        );
        assert_eq!(selections_in(&[]), Selections::default());
    }
}
