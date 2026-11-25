//! Fixtures shared by the session submodules' colocated tests.

use std::sync::Arc;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::Event;
#[cfg(test)]
use horizon_agent::contract::{ApprovalKind, ApprovalRequest, ToolCallId};
#[cfg(test)]
use horizon_agent::hosting::AgentWireEvent;
use horizon_agent::registry::ProviderRegistry;
#[cfg(test)]
use horizon_agent::tools::ApprovalCandidate;

use super::state::AgentdState;

/// In-memory session configuration, independent of the developer's environment.
/// Persistence tests install their own writer and paths explicitly.
pub fn test_config() -> AgentConfig {
    use horizon_agent::config::{
        AgentPersistenceConfig, NamedProviderConfig, ProvidersTable, RigAgentConfig,
    };
    let rig = RigAgentConfig {
        packages: horizon_agent::packages::Packages::default(),
        ..RigAgentConfig::default()
    };
    AgentConfig {
        auxiliary: None,
        decision: None,
        providers: ProvidersTable {
            entries: vec![NamedProviderConfig {
                name: "default".into(),
                vendor: rig.vendor,
                base_url: rig.base_url.clone(),
                api_key_env: rig.api_key_env.clone(),
                api_key_present: rig.api_key_present,
            }],
            default_model: None,
        },
        rig,
        persistence: AgentPersistenceConfig {
            sessions_dir: Default::default(),
            legacy_event_log: None,
            duckdb_path: None,
        },
        tools: Default::default(),
    }
}

pub fn state_from_config(
    config: AgentConfig,
    trusted: Vec<std::path::PathBuf>,
) -> Arc<AgentdState> {
    Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(config.clone()),
        config,
        None,
        None,
        Vec::new(),
        trusted,
    ))
}

pub fn test_state() -> Arc<AgentdState> {
    state_from_config(test_config(), Vec::new())
}

#[cfg(test)]
pub(super) fn judge_candidate(call_id: &str) -> ApprovalCandidate {
    let request = horizon_agent::contract::ToolCallRequest {
        call_id: ToolCallId(call_id.to_string()),
        tool_id: "mock_approval_required".to_string(),
        input: serde_json::json!({}).into(),
        occurrence_id: horizon_agent::contract::OccurrenceId(call_id.to_string()),
    };
    ApprovalCandidate {
        approval: ApprovalRequest {
            call_id: request.call_id.clone(),
            reason: "test approval".to_string(),
            kind: ApprovalKind::Standard,
            occurrence_id: horizon_agent::contract::OccurrenceId(request.call_id.0.clone()),
        },
        request,
    }
}

#[cfg(test)]
pub(super) fn drain_events(rx: &mut tokio::sync::mpsc::Receiver<AgentWireEvent>) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(wire_event) = rx.try_recv() {
        if let AgentWireEvent::Event(event) = wire_event {
            events.push(event);
        }
    }
    events
}

/// Override only the provider properties relevant to the scenario.
pub fn state_with_rig_config(api_key_present: bool, model: &str) -> Arc<AgentdState> {
    state_from_config(rig_config(api_key_present, model), Vec::new())
}

fn rig_config(api_key_present: bool, model: &str) -> AgentConfig {
    let mut config = test_config();
    config.rig.api_key_present = api_key_present;
    config.rig.model = model.to_string();
    config.providers.entries[0].api_key_present = api_key_present;
    config.providers.default_model = Some(horizon_agent::config::ModelPair {
        provider: "default".into(),
        model: model.to_string(),
        effort: None,
    });
    config
}

/// [`state_with_rig_config`] with what an extension adds to every session.
pub fn state_with_packages(
    api_key_present: bool,
    model: &str,
    packages: horizon_agent::packages::Packages,
) -> Arc<AgentdState> {
    let mut config = rig_config(api_key_present, model);
    config.rig.packages = packages;
    state_from_config(config, Vec::new())
}

pub fn state_with_trusted_projects(trusted: Vec<std::path::PathBuf>) -> Arc<AgentdState> {
    state_from_config(test_config(), trusted)
}

pub fn record(
    session_id: horizon_agent::contract::SessionId,
    event: Event,
) -> horizon_agent::persistence::event_log::Record {
    horizon_agent::persistence::event_log::Record {
        schema: "horizon.agent.event_log".into(),
        version: 5,
        event_id: uuid::Uuid::new_v4().to_string(),
        parent_id: None,
        sequence: 1,
        session_id,
        turn_id: None,
        provider_id: None,
        role_id: None,
        session_context: None,
        event_kind: horizon_agent::contract::event_kind(&event).into(),
        event,
        provider_payload: None,
        created_at_unix_ms: 1,
    }
}

#[test]
fn session_fixtures_do_not_read_host_provider_or_persistence_settings() {
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("OPENAI_API_KEY", "fixture-only") };
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HORIZON_AGENT_CLEARING_THRESHOLD_PCT", "1") };
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HORIZON_AGENT_SESSIONS_DIR", "host-sessions") };
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HORIZON_AGENT_STATE_DB", "host-state.duckdb") };
    for state in [
        test_state(),
        state_with_rig_config(false, "test-model"),
        state_with_trusted_projects(Vec::new()),
    ] {
        let config = state.agent_config.lock().unwrap();
        assert!(!config.rig.api_key_present);
        assert_eq!(config.rig.base_url, None);
        assert_eq!(
            config.rig.clearing_threshold_pct,
            horizon_agent::config::RigAgentConfig::default().clearing_threshold_pct
        );
        assert!(config.persistence.sessions_dir.as_os_str().is_empty());
        assert_eq!(config.persistence.duckdb_path, None);
        assert!(!config.providers.entries[0].api_key_present);
    }
}

/// Single-execution fixtures use the call label as their occurrence label.
#[cfg(test)]
pub(super) fn tool_identity(call_id: &ToolCallId) -> horizon_agent::contract::ToolCallIdentity {
    horizon_agent::contract::ToolCallIdentity {
        call_id: call_id.clone(),
        occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
    }
}
