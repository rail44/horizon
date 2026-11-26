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

/// A session to start ([`ProviderRegistry::start_session`]).
pub struct NewSession {
    pub session_id: SessionId,
    /// Validated against the registry's packages before any provider sees it.
    pub role_id: Option<RoleId>,
    /// The session's real working directory; `None` when it has none.
    pub workspace_root: Option<PathBuf>,
    /// What the session already did, for a resumed one.
    pub history: Vec<Event>,
    /// Whether the session's project is trusted to give it instructions.
    pub trusted_project: bool,
    /// The session's modules (`modules::Modules::compose`); `None` has the
    /// provider compose the default ones.
    pub modules: Option<crate::modules::Modules>,
    /// The session's packages with the tools its client handed it; `None`
    /// has the provider use its own.
    pub packages: Option<crate::packages::Packages>,
}

pub(crate) trait Provider: Send + Sync {
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
    /// `horizon-agent-host` to surface a session's model to the UI from
    /// session start, ahead of any turn's `Event::ProviderRequestSent` --
    /// see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    fn resolved_model(&self, role_id: Option<&RoleId>) -> Option<String>;
}

/// Called on a mock session's thread once a slow turn's first word is out
/// ([`ProviderRegistry::hold_mock_streams`]).
pub type MockStreamHold = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
    /// What packages add (`config.rig.packages`), for validating a role.
    packages: crate::packages::Packages,
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
    /// Test-only convenience: the built-in providers over the default
    /// configuration.
    #[cfg(test)]
    pub(crate) fn builtin() -> Self {
        Self::builtin_with_config(AgentConfig::from_env_and_providers(
            vec![crate::config::NamedProviderConfig::default()],
            None,
        ))
    }

    /// One rig provider per resolved `[[providers]]` entry, registered under
    /// its own id, plus the mock provider. The default entry is ALSO
    /// registered under the standing shell-facing id
    /// ([`Self::default_provider_id`]'s `builtin.agent.rig`), so a session
    /// spawn that names no provider keeps starting on the default provider
    /// unchanged -- including a caller (`horizon-agent-host`'s own test seams)
    /// that adjusts `config.rig` after construction, which is why the
    /// default entry passes `config.rig` itself rather than re-resolving:
    /// the state's own view is authoritative for the default entry. An
    /// entry whose key variable is unset still registers (available=false
    /// to `list_providers`, deterministic fallback turns) -- "registered
    /// but unavailable", never a failed startup.
    pub fn builtin_with_config(config: AgentConfig) -> Self {
        let mut registry = Self {
            packages: config.rig.packages.clone(),
            ..Self::default()
        };
        #[cfg(any(test, feature = "test-fixtures"))]
        registry.insert_under(
            crate::providers::mock::MockProvider::id(),
            Arc::new(crate::providers::mock::MockProvider::new()),
        );
        let table = config.providers.clone();
        let default_name = table.default_provider_name().map(str::to_string);
        for entry in &table.entries {
            let id = named_rig_provider_id(&entry.name);
            let is_default = default_name.as_deref() == Some(entry.name.as_str());
            let mut provider_config = if is_default {
                config.rig.clone()
            } else {
                entry.resolved()
            };
            provider_config.decision = config.decision.clone();
            provider_config.packages = config.rig.packages.clone();
            provider_config.data_home = Some(config.persistence.data_home.clone());
            let provider = Arc::new(crate::providers::rig::Provider::for_entry(provider_config));
            registry.insert_under(id, provider.clone());
            if is_default {
                registry.insert_under(ProviderId("builtin.agent.rig".to_string()), provider);
            }
        }
        // No resolvable `default_model`: the shell-facing id still answers,
        // with `config.rig`'s empty model, so a session spawn that names no
        // provider starts in the deterministic fallback instead of failing.
        if default_name.is_none() && !table.entries.is_empty() {
            let id = ProviderId("builtin.agent.rig".to_string());
            registry.insert_under(
                id.clone(),
                Arc::new(crate::providers::rig::Provider::for_entry(
                    crate::config::RigAgentConfig {
                        decision: config.decision.clone(),
                        data_home: Some(config.persistence.data_home.clone()),
                        ..config.rig.clone()
                    },
                )),
            );
        }
        // A surface with no entries at all (the never-fail shape
        // `AgentConfig::from_env_and_providers` still carries) has nothing
        // to register under the shell-facing id -- session spawns naming no
        // provider fail loudly the way an unknown provider id already
        // does, rather than starting a role-less session on nothing.
        registry
    }

    /// Has the mock provider's slow turns wait on `hold` once their first
    /// word is out: a test reattaches while a turn is surely streaming. A
    /// build without test fixtures has no mock provider, and this does
    /// nothing.
    pub fn hold_mock_streams(&mut self, hold: MockStreamHold) {
        #[cfg(any(test, feature = "test-fixtures"))]
        self.insert_under(
            crate::providers::mock::MockProvider::id(),
            Arc::new(crate::providers::mock::MockProvider::holding(hold)),
        );
        #[cfg(not(any(test, feature = "test-fixtures")))]
        drop(hold);
    }

    /// Registers `provider` under `id`; the default entry is registered
    /// twice, under its own named id and the shell-facing one
    /// ([`Self::builtin_with_config`]).
    pub(crate) fn insert_under(&mut self, id: ProviderId, provider: Arc<dyn Provider>) {
        self.providers.insert(id, provider);
    }

    /// The standing shell-facing id a session spawn resolves to when it
    /// names no provider: always the *default* entry, whichever `[[providers]]`
    /// surface (or the legacy `[provider]` fold-in) built the registry.
    pub fn default_provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    /// Whether a provider is registered under `provider_id` — checked
    /// before spawning a session against a configured name, so an unknown
    /// one fails at the caller instead of inside the session thread.
    pub fn contains(&self, provider_id: &ProviderId) -> bool {
        self.providers.contains_key(provider_id)
    }

    /// Starts a session, forwarding `role_id` to whichever provider is
    /// registered under `provider_id`. Validates `role_id` *before*
    /// dispatching to the provider -- an unresolvable role id returns
    /// `None` here exactly like an unknown `provider_id` does, so a caller
    /// that already treats `None` as "fail loudly, don't start a role-less
    /// session instead" (see `roles`'s module doc; `horizon-agent-host`'s
    /// `session::run_session` is the one production caller) gets that
    /// behavior for both failure modes without extra plumbing. This is the
    /// single choke point every session start goes through, so a role is
    /// validated the same way regardless of which provider ends up running
    /// it -- including the mock provider, which otherwise accepts and
    /// ignores `role_id` entirely (see `providers::mock`).
    pub fn start_session(
        &self,
        provider_id: &ProviderId,
        session: NewSession,
    ) -> Option<SessionHandle> {
        if let Some(role_id) = &session.role_id {
            self.packages.resolve_role(role_id)?;
        }
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartSession {
                session_id: session.session_id,
                provider_id: provider_id.clone(),
                role_id: session.role_id,
                workspace_root: session.workspace_root,
                history: session.history,
                trusted_project: session.trusted_project,
                modules: session.modules,
                packages: session.packages,
            })
        })
    }

    /// Delegates to the named provider's [`Provider::resolved_model`].
    /// `None` for an unknown `provider_id` too -- same "nothing to report"
    /// shape as an unresolvable model, since the caller
    /// (`horizon-agent-host`'s session spawn) already handles an unknown
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
    use crate::config::{AgentPersistenceConfig, NamedProviderConfig, ProvidersTable, Vendor};

    fn named_entry(
        name: &str,
        kind: Vendor,
        api_key_env: &str,
        api_key_present: bool,
    ) -> NamedProviderConfig {
        NamedProviderConfig {
            name: name.to_string(),
            vendor: kind,
            base_url: None,
            api_key_env: api_key_env.to_string(),
            api_key_present,
        }
    }

    /// The two-provider completion condition: one entry with its key
    /// variable unset must not break the registry build — both entries
    /// register (the unavailable one stays registered, "grayed out, not
    /// hidden"), the default entry answers under the standing shell-facing
    /// id with the default model, and an entry nothing selected a model
    /// for resolves none.
    #[test]
    fn two_entries_register_independently_of_each_others_key_presence() {
        let agent_config = crate::config::AgentConfig {
            auxiliary: None,
            decision: None,
            rig: crate::config::RigAgentConfig {
                api_key_present: true,
                model: "test-model".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: vec![
                    named_entry("openai", Vendor::openai(), "OPENAI_API_KEY", true),
                    named_entry(
                        "claude",
                        Vendor::resolve("anthropic").unwrap(),
                        "ANTHROPIC_API_KEY",
                        false,
                    ),
                ],
                default_model: Some(crate::config::ModelPair {
                    provider: "openai".to_string(),
                    model: "test-model".to_string(),
                    effort: None,
                }),
            },
            persistence: AgentPersistenceConfig {
                sessions_dir: std::path::PathBuf::from("/tmp/horizon-registry-test-sessions"),
                legacy_event_log: None,
                duckdb_path: None,
                data_home: std::path::PathBuf::from("/tmp/horizon-registry-test-data"),
            },
            tools: crate::config::AgentToolsConfig::default(),
        };

        let registry = ProviderRegistry::builtin_with_config(agent_config);
        // Both entries registered under their own named ids, plus the mock.
        for name in ["openai", "claude"] {
            assert!(
                registry
                    .providers
                    .contains_key(&named_rig_provider_id(name)),
                "entry {name} must be registered"
            );
        }
        assert!(
            registry
                .providers
                .contains_key(&ProviderId("builtin.agent.mock".to_string()))
        );

        // The default entry ALSO answers under the standing shell-facing id
        // — a session spawn that names no provider keeps starting on the
        // default provider unchanged.
        let openai_id = named_rig_provider_id("openai");
        assert_eq!(
            registry.resolved_model(&registry.default_provider_id(), None),
            Some("test-model".to_string())
        );
        // The named id of the entry `default_model` names runs the same
        // config.
        assert_eq!(
            registry.resolved_model(&openai_id, None),
            Some("test-model".to_string())
        );

        // The key-less entry registered but resolves nothing: unavailable
        // ("grayed out"), not a failed startup and not invisible.
        assert_eq!(
            registry.resolved_model(&named_rig_provider_id("claude"), None),
            None
        );
    }
}
