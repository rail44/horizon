//! OpenAI-compatible connections for titles and approval judgments, built
//! from the config file's `small_model` pair.
//! Clients belong to an accepted configuration, never to the whole process.

use std::sync::OnceLock;

use rig_core::providers::openai;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxiliaryConfig {
    pub base_url: Option<String>,
    pub api_key_env: String,
    /// The model id titles and approval judgments run on.
    pub model: String,
}

impl AuxiliaryConfig {
    /// The file layer validates the selected provider's kind. Only the secret's
    /// environment variable name crosses this boundary.
    pub fn from_env(base_url: Option<String>, api_key_env: String, model: String) -> Self {
        Self {
            base_url: std::env::var(crate::config::OPENAI_BASE_URL_VAR)
                .ok()
                .or(base_url),
            api_key_env,
            model,
        }
    }
}

pub struct AuxiliaryClient {
    config: AuxiliaryConfig,
    client: OnceLock<Option<openai::CompletionsClient>>,
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

    pub(crate) fn completion_client(&self) -> anyhow::Result<openai::CompletionsClient> {
        self.client
            .get_or_init(|| {
                let api_key = std::env::var(&self.config.api_key_env).ok()?;
                let mut builder = openai::CompletionsClient::builder().api_key(&api_key);
                if let Some(base_url) = &self.config.base_url {
                    builder = builder.base_url(base_url);
                }
                builder.build().ok()
            })
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "auxiliary client unavailable ({} unset, or client build failed)",
                    self.config.api_key_env,
                )
            })
    }
}
