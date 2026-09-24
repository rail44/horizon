//! Resolve incoming selections once; record only the provider's applied feedback.

use horizon_agent::contract::{Command, ProviderEvent, SessionId};

use super::state::{lock_unpoisoned, AgentdState};

pub(super) fn resolve(state: &AgentdState, provider: &str, model: &str) -> Result<Command, String> {
    let config = lock_unpoisoned(&state.agent_config);
    horizon_agent::config::resolve_model_selection(&config.providers, &config.moa, provider, model)
        .map(|selection| Command::ApplySessionModel(Box::new(selection)))
}

pub(super) fn record_applied(state: &AgentdState, session_id: SessionId, event: &ProviderEvent) {
    if event.session_model.is_none() && event.session_selection.is_none() {
        return;
    }
    let mut sessions = lock_unpoisoned(&state.sessions);
    if let Some(entry) = sessions.get_mut(&session_id) {
        if let Some(model) = &event.session_model {
            entry.model = Some(model.clone());
        }
        if let Some(selection) = &event.session_selection {
            entry.selection = Some(selection.clone());
        }
    }
}
