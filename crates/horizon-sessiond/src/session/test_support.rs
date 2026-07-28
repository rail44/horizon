//! Fixtures shared by the session submodules' colocated tests.

use std::sync::Arc;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{ApprovalKind, ApprovalRequest, Event, ProviderRegistry, ToolCallId};
use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
use horizon_agent::tools::ApprovalCandidate;
use horizon_agent::wire::AgentWireEvent;

use super::state::SessiondState;

pub(super) fn judge_test_state() -> Arc<SessiondState> {
    let agent_config = AgentConfig::from_env_and_provider(None, None);
    Arc::new(SessiondState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
    ))
}

pub(super) fn judge_candidate(call_id: &str) -> ApprovalCandidate {
    let request = horizon_agent::contract::ToolCallRequest {
        call_id: ToolCallId(call_id.to_string()),
        tool_id: "mock.approval_required".to_string(),
        input: serde_json::json!({}).into(),

        occurrence_id: None,
    };
    ApprovalCandidate {
        approval: ApprovalRequest {
            call_id: request.call_id.clone(),
            reason: "test approval".to_string(),
            kind: ApprovalKind::Standard,

            occurrence_id: None,
        },
        request,
    }
}

pub(super) fn drain_events(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentWireEvent>,
) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(wire_event) = rx.try_recv() {
        if let AgentWireEvent::Event(event) = wire_event {
            events.push(event);
        }
    }
    events
}

/// Builds a hermetic [`SessiondState`] with an explicit, env-independent
/// `RigAgentConfig` (never `AgentConfig::from_env_and_provider`'s real
/// env vars -- a developer's own `OPENAI_API_KEY` must never leak into
/// this test's expectations). Tests observing sends subscribe the
/// session id under test via [`Connection::subscribe_agent`].
pub(super) fn state_with_rig_config(openai_enabled: bool, model: &str) -> Arc<SessiondState> {
    let mut agent_config = AgentConfig::from_env_and_provider(None, None);
    agent_config.rig.openai_enabled = openai_enabled;
    agent_config.rig.model = model.to_string();
    Arc::new(SessiondState::new(
        ProviderRegistry::builtin_with_config(
            agent_config.clone(),
            SharedDuckdbStore::unavailable(),
        ),
        agent_config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
    ))
}
