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

/// A named `[[providers]]` entry's registry id — the entry's `name`
/// namespaced under the rig family, so it can never collide with the
/// standing shell-facing default id (`builtin.agent.rig`) or the mock
/// provider's (`builtin.agent.mock`). The default entry is additionally
/// registered under the shell-facing id (see
/// [`ProviderRegistry::builtin_with_config`]).
pub fn named_rig_provider_id(name: &str) -> ProviderId {
    ProviderId(format!("builtin.agent.rig.{name}"))
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
    /// registry -- and the rig providers it constructs -- is built at
    /// process startup, before the event log's writer thread (and
    /// therefore any real DuckDB store) exists yet.
    ///
    /// One rig provider per resolved `[[providers]]` entry, registered under
    /// its own id, plus the mock provider. The default entry is ALSO
    /// registered under the standing shell-facing id
    /// ([`Self::default_provider_id`]'s `builtin.agent.rig`), so a session
    /// spawn that names no provider keeps starting on the default provider
    /// unchanged -- including a caller (`horizon-agentd`'s own test seams)
    /// that adjusts `config.rig` after construction, which is why the
    /// default entry passes `config.rig` itself rather than re-resolving:
    /// the state's own view is authoritative for the default entry. An
    /// entry whose key variable is unset still registers (available=false
    /// to `list_providers`, deterministic fallback turns) -- "registered
    /// but unavailable", never a failed startup.
    pub fn builtin_with_config(
        config: AgentConfig,
        duckdb_cell: crate::persistence::projection::duckdb::SharedDuckdbStore,
    ) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::providers::mock::MockProvider::new()));
        let table = config.providers.clone();
        for entry in &table.entries {
            let id = named_rig_provider_id(&entry.name);
            let provider = if entry.name == table.default_name {
                crate::providers::rig::Provider::for_entry(
                    id.clone(),
                    config.rig.clone(),
                    table.clone(),
                    duckdb_cell.clone(),
                )
            } else {
                crate::providers::rig::Provider::for_entry(
                    id.clone(),
                    entry.resolved(),
                    table.clone(),
                    duckdb_cell.clone(),
                )
            };
            let provider = Arc::new(provider);
            registry.insert_under(id, provider.clone());
            if entry.name == table.default_name {
                registry.insert_under(ProviderId("builtin.agent.rig".to_string()), provider);
            }
        }
        // A surface with no entries at all (the never-fail shape
        // `AgentConfig::from_env_and_providers` still carries) has nothing
        // to register under the shell-facing id -- session spawns naming no
        // provider fail loudly the way an unknown provider id already
        // does, rather than starting a role-less session on nothing.
        registry
    }

    pub(crate) fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    /// Registers under an explicit id — [`Self::builtin_with_config`]'s
    /// dual-registration of the default entry (its own named id AND the
    /// standing shell-facing one) is the one caller.
    pub(crate) fn insert_under(&mut self, id: ProviderId, provider: Arc<dyn Provider>) {
        self.providers.insert(id, provider);
    }

    /// The standing shell-facing id a session spawn resolves to when it
    /// names no provider: always the *default* entry, whichever `[[providers]]`
    /// surface (or the legacy `[provider]` fold-in) built the registry.
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
        trusted_project: bool,
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
                trusted_project,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentPersistenceConfig, NamedProviderConfig, ProviderKind, ProvidersTable,
    };
    use crate::persistence::projection::duckdb::SharedDuckdbStore;

    fn named_entry(
        name: &str,
        kind: ProviderKind,
        api_key_env: &str,
        api_key_present: bool,
        models: Vec<(&str, &str)>,
    ) -> NamedProviderConfig {
        NamedProviderConfig {
            name: name.to_string(),
            kind,
            base_url: None,
            api_key_env: api_key_env.to_string(),
            api_key_present,
            models: models
                .into_iter()
                .map(|(alias, id)| (alias.to_string(), id.to_string()))
                .collect(),
        }
    }

    /// The two-provider completion condition: one entry with its key
    /// variable unset must not break the registry build — both entries
    /// register (the unavailable one stays registered, "grayed out, not
    /// hidden"), the default entry answers under the standing shell-facing
    /// id, and each entry resolves its own model.
    #[test]
    fn two_entries_register_independently_of_each_others_key_presence() {
        let agent_config = crate::config::AgentConfig {
            rig: crate::config::RigAgentConfig {
                api_key_present: true,
                model: "test-model".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: vec![
                    named_entry(
                        "openai",
                        ProviderKind::OpenAiCompatible,
                        "OPENAI_API_KEY",
                        true,
                        vec![("fast", "m-fast")],
                    ),
                    named_entry(
                        "claude",
                        ProviderKind::Anthropic,
                        "ANTHROPIC_API_KEY",
                        false,
                        vec![("opus", "m-opus")],
                    ),
                ],
                default_name: "openai".to_string(),
            },
            persistence: AgentPersistenceConfig {
                event_log_path: std::path::PathBuf::from("/tmp/horizon-registry-test-events.jsonl"),
                duckdb_path: None,
            },
            tools: crate::config::AgentToolsConfig::default(),
        };

        let registry =
            ProviderRegistry::builtin_with_config(agent_config, SharedDuckdbStore::unavailable());
        // Both entries registered under their own named ids, plus the mock.
        for name in ["openai", "claude"] {
            assert!(
                registry
                    .providers
                    .contains_key(&named_rig_provider_id(name)),
                "entry {name} must be registered"
            );
        }
        assert!(registry
            .providers
            .contains_key(&ProviderId("builtin.agent.mock".to_string())));

        // The default entry ALSO answers under the standing shell-facing id
        // — a session spawn that names no provider keeps starting on the
        // default provider unchanged.
        let openai_id = named_rig_provider_id("openai");
        assert_eq!(
            registry.resolved_model(&registry.default_provider_id(), None),
            Some("test-model".to_string())
        );
        // The named id resolves the same entry — self-consistent against
        // whatever env precedence did to its base URL/model.
        assert_eq!(
            registry.resolved_model(&openai_id, None),
            Some(registry.resolved_model(&openai_id, None).unwrap())
        );

        // The key-less entry registered but resolves nothing: unavailable
        // ("grayed out"), not a failed startup and not invisible.
        assert_eq!(
            registry.resolved_model(&named_rig_provider_id("claude"), None),
            None
        );
    }
}
