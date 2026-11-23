//! Resolve incoming selections once; record only the provider's applied feedback.

use horizon_agent::contract::{Command, ProviderEvent, SessionId};

use super::state::{AgentdState, lock_unpoisoned};

pub(super) fn resolve(state: &AgentdState, provider: &str, model: &str) -> Result<Command, String> {
    let config = lock_unpoisoned(&state.agent_config);
    horizon_agent::config::resolve_model_selection(&config.providers, provider, model)
        .map(|selection| Command::ApplySessionModel(Box::new(selection)))
}

pub(super) fn record_applied(state: &AgentdState, session_id: SessionId, event: &ProviderEvent) {
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
