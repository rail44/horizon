use std::{collections::HashMap, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use super::mock::MockAgentProvider;
use super::types::{AgentCommand, AgentProviderEvent, AgentProviderId, StartAgentSession};
use crate::agent_config::AgentConfig;
use crate::workspace::SessionId;

#[derive(Clone)]
pub struct AgentSessionHandle {
    commands: Sender<AgentCommand>,
    events: Receiver<AgentProviderEvent>,
}

impl AgentSessionHandle {
    pub fn new(commands: Sender<AgentCommand>, events: Receiver<AgentProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub fn sender(&self) -> Sender<AgentCommand> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<AgentProviderEvent> {
        self.events.clone()
    }
}

pub trait AgentProvider: Send + Sync {
    fn provider_id(&self) -> AgentProviderId;
    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle;
}

#[derive(Clone, Default)]
pub struct AgentProviderRegistry {
    providers: HashMap<AgentProviderId, Arc<dyn AgentProvider>>,
}

impl AgentProviderRegistry {
    pub fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env())
    }

    pub fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(MockAgentProvider::new()));
        registry.insert(Arc::new(crate::agent::rig::RigAgentProvider::new(
            config.rig,
            config.persistence.duckdb_path,
        )));
        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn AgentProvider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub fn default_provider_id(&self) -> AgentProviderId {
        AgentProviderId("builtin.agent.rig".to_string())
    }

    pub fn start_session(
        &self,
        provider_id: &AgentProviderId,
        session_id: SessionId,
    ) -> Option<AgentSessionHandle> {
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartAgentSession {
                session_id,
                provider_id: provider_id.clone(),
            })
        })
    }
}
