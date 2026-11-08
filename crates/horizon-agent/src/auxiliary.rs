//! OpenAI-compatible connections for titles, and for approval judgments when
//! no decision model is available, built from the config file's
//! `small_model` pair.
//! Clients belong to an accepted configuration, never to the whole process.

use std::sync::OnceLock;

use rig_core::providers::openai;

/// The `small_model` pair's openai-compatible entry and model id. The file
/// layer validates the entry's kind; only the secret's environment variable
/// name crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxiliaryConfig {
    /// The entry's `base_url`, used as written.
    pub base_url: Option<String>,
    pub api_key_env: String,
    /// The model id titles, and the fallback approval judge, run on.
    pub model: String,
}

pub struct AuxiliaryClient {
    config: AuxiliaryConfig,
    client: OnceLock<Option<openai::OpenAI>>,
}

impl AuxiliaryClient {
    pub fn new(config: AuxiliaryConfig) -> Self {
        Self {
            config,
            client: OnceLock::new(),
        }
    }

    /// The model id this connection's calls run on (`small_model`).
    pub(crate) fn model(&self) -> &str {
        &self.config.model
    }

    pub(crate) fn completion_client(&self) -> anyhow::Result<openai::OpenAI> {
        self.client
            .get_or_init(|| {
                let api_key = std::env::var(&self.config.api_key_env).ok()?;
                // rig 0.44's OpenAI dialect defaults to `/responses`; every
                // configured endpoint speaks `/chat/completions`.
                let mut config =
                    openai::wire::OpenAIConfig::new(api_key).with_route(openai::Route::Chat);
                if let Some(base_url) = &self.config.base_url {
                    config = config.with_base_url(base_url);
                }
                Some(config.client())
            })
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "auxiliary client unavailable ({} unset)",
                    self.config.api_key_env,
                )
            })
    }
}
