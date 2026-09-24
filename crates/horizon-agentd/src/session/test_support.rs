//! Fixtures shared by the session submodules' colocated tests.

use std::sync::Arc;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{ApprovalKind, ApprovalRequest, Event, ToolCallId};
use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
use horizon_agent::registry::ProviderRegistry;
use horizon_agent::tools::ApprovalCandidate;
use horizon_agent::wire::AgentWireEvent;

use super::state::AgentdState;

/// In-memory session configuration, independent of the developer's environment.
/// Persistence tests install their own writer and paths explicitly.
pub(crate) fn test_config() -> AgentConfig {
    use horizon_agent::config::{
        AgentPersistenceConfig, NamedProviderConfig, ProvidersTable, RigAgentConfig,
    };
    let rig = RigAgentConfig::default();
    AgentConfig {
        providers: ProvidersTable {
            entries: vec![NamedProviderConfig {
                name: "default".into(),
                kind: rig.kind,
                base_url: rig.base_url.clone(),
                api_key_env: rig.api_key_env.clone(),
                api_key_present: rig.api_key_present,
                default_model: Some(rig.model.clone()),
            }],
            default_name: "default".into(),
        },
        rig,
        moa: Default::default(),
        persistence: AgentPersistenceConfig {
            event_log_path: Default::default(),
            duckdb_path: None,
        },
        tools: Default::default(),
    }
}

fn state_from_config(config: AgentConfig, trusted: Vec<std::path::PathBuf>) -> Arc<AgentdState> {
    Arc::new(AgentdState::new(
        ProviderRegistry::builtin_with_config(config.clone(), SharedDuckdbStore::unavailable()),
        config,
        None,
        SharedDuckdbStore::unavailable(),
        None,
        Vec::new(),
        trusted,
    ))
}

pub(crate) fn test_state() -> Arc<AgentdState> {
    state_from_config(test_config(), Vec::new())
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

/// Override only the provider properties relevant to the scenario.
pub(crate) fn state_with_rig_config(api_key_present: bool, model: &str) -> Arc<AgentdState> {
    let mut config = test_config();
    config.rig.api_key_present = api_key_present;
    config.rig.model = model.to_string();
    config.providers.entries[0].api_key_present = api_key_present;
    config.providers.entries[0].default_model = Some(model.to_string());
    state_from_config(config, Vec::new())
}

pub(crate) fn state_with_trusted_projects(trusted: Vec<std::path::PathBuf>) -> Arc<AgentdState> {
    state_from_config(test_config(), trusted)
}

#[test]
fn session_fixtures_do_not_read_host_provider_or_persistence_settings() {
    std::env::set_var("OPENAI_API_KEY", "fixture-only");
    std::env::set_var("OPENAI_BASE_URL", "https://fixture.invalid");
    std::env::set_var("HORIZON_RIG_MODEL", "host-model");
    std::env::set_var("HORIZON_AGENT_CLEARING_THRESHOLD_PCT", "1");
    std::env::set_var("HORIZON_AGENT_EVENT_LOG", "host-events.jsonl");
    std::env::set_var("HORIZON_AGENT_STATE_DB", "host-state.duckdb");
    for state in [
        test_state(),
        state_with_rig_config(false, "test-model"),
        state_with_trusted_projects(Vec::new()),
    ] {
        let config = state.agent_config.lock().unwrap();
        assert!(!config.rig.api_key_present);
        assert_eq!(config.rig.base_url, None);
        assert_ne!(config.rig.model, "host-model");
        assert_eq!(
            config.rig.clearing_threshold_pct,
            horizon_agent::config::RigAgentConfig::default().clearing_threshold_pct
        );
        assert!(config.persistence.event_log_path.as_os_str().is_empty());
        assert_eq!(config.persistence.duckdb_path, None);
        assert!(!config.providers.entries[0].api_key_present);
    }
}
