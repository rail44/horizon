use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use crate::config::AgentConfig;
use crate::contract::{Command, Event, ProviderEvent, ProviderId, SessionId, StartSession};
use crate::roles::RoleId;

#[derive(Clone)]
pub struct SessionHandle {
    commands: Sender<Command>,
    events: Receiver<ProviderEvent>,
}

impl SessionHandle {
    pub fn new(commands: Sender<Command>, events: Receiver<ProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<ProviderEvent> {
        self.events.clone()
    }
}

pub(crate) trait Provider: Send + Sync {
    fn provider_id(&self) -> ProviderId;
    fn start_session(&self, request: StartSession) -> SessionHandle;
    /// The model id a session with this `role_id` would run with, resolved
    /// the same way [`Self::start_session`] resolves it (role override, else
    /// the provider's own configured default) but without spinning up a
    /// session -- pure and synchronous, so a caller can learn a session's
    /// model before (or without) starting one. `None` when this provider has
    /// no meaningful single model (e.g. the mock provider) or isn't actually
    /// going to call one (the rig provider's deterministic fallback mode,
    /// used when no API key is configured -- see
    /// `providers::rig::Provider::resolved_model`'s doc comment). Used by
    /// `horizon-agentd` to surface a session's model to the UI from
    /// session start, ahead of any turn's `Event::ProviderRequestSent` --
    /// see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    fn resolved_model(&self, role_id: Option<&RoleId>) -> Option<String>;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Test-only convenience: no real event-log writer exists behind this
    /// registry, so the rig provider gets an already-resolved-to-`None`
    /// [`crate::persistence::projection::duckdb::SharedDuckdbStore`]
    /// (`SharedDuckdbStore::unavailable`) -- reads through it return
    /// immediately with no history, and never block, exactly like the
    /// pre-recall behavior of a provider constructed with no DuckDB path.
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        Self::builtin_with_config(
            AgentConfig::from_env_and_provider(None, None),
            crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
        )
    }

    /// `duckdb_cell` is shared with (a clone of) whatever else in the
    /// process needs the same live DuckDB projection handle once it exists
    /// (`horizon-agentd`'s `AgentdState`, for the recall tools) -- see
    /// `persistence::projection::duckdb::SharedDuckdbStore`'s doc comment.
    /// It's threaded in here (rather than resolved internally) because this
    /// registry -- and the rig provider it constructs -- is built at
    /// process startup, before the event log's writer thread (and
    /// therefore any real DuckDB store) exists yet.
    pub fn builtin_with_config(
        config: AgentConfig,
        duckdb_cell: crate::persistence::projection::duckdb::SharedDuckdbStore,
    ) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::providers::mock::MockProvider::new()));
        registry.insert(Arc::new(crate::providers::rig::Provider::new(
            config.rig,
            duckdb_cell,
        )));
        registry
    }

    pub(crate) fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub fn default_provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    /// Starts a session, forwarding `role_id` to whichever provider is
    /// registered under `provider_id`. Validates `role_id` *before*
    /// dispatching to the provider -- an unresolvable role id returns
    /// `None` here exactly like an unknown `provider_id` does, so a caller
    /// that already treats `None` as "fail loudly, don't start a role-less
    /// session instead" (see `roles`'s module doc; `horizon-agentd`'s
    /// `session::run_session` is the one production caller) gets that
    /// behavior for both failure modes without extra plumbing. This is the
    /// single choke point every session start goes through, so a role is
    /// validated the same way regardless of which provider ends up running
    /// it -- including the mock provider, which otherwise accepts and
    /// ignores `role_id` entirely (see `providers::mock`).
    pub fn start_session(
        &self,
        provider_id: &ProviderId,
        session_id: SessionId,
        role_id: Option<RoleId>,
        workspace_root: Option<PathBuf>,
        history: Vec<Event>,
    ) -> Option<SessionHandle> {
        if let Some(role_id) = &role_id {
            crate::roles::resolve(role_id)?;
        }
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartSession {
                session_id,
                provider_id: provider_id.clone(),
                role_id,
                workspace_root,
                history,
            })
        })
    }

    /// Delegates to the named provider's [`Provider::resolved_model`].
    /// `None` for an unknown `provider_id` too -- same "nothing to report"
    /// shape as an unresolvable model, since the caller
    /// (`horizon-agentd`'s session spawn) already handles an unknown
    /// provider as a hard session-start failure separately (see
    /// [`Self::start_session`]).
    pub fn resolved_model(
        &self,
        provider_id: &ProviderId,
        role_id: Option<&RoleId>,
    ) -> Option<String> {
        self.providers.get(provider_id)?.resolved_model(role_id)
    }
}
