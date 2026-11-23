//! Connections on OpenAI's wire for titles, and for approval judgments when
//! no decision model is available, built from the config file's
//! `small_model` pair.
//! Clients belong to an accepted configuration, never to the whole process.

use std::sync::OnceLock;

use rig_core::completion::{GenerationOptions, OnUnsupported, Reasoning};
use rig_core::providers::{openai, registry::ProviderConfig};

use crate::config::{Vendor, Wire};

/// The `small_model` pair's entry and model id, on a vendor speaking
/// OpenAI's wire ([`Self::resolve`]): the judge reads the logprobs of an
/// OpenAI chat completion. Only the secret's environment variable name
/// crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxiliaryConfig {
    /// The entry's vendor, whose dialect the requests are spelled in.
    pub vendor: Vendor,
    /// The entry's `base_url`, used as written; `None` is the vendor's own.
    pub base_url: Option<String>,
    pub api_key_env: String,
    /// The model id titles, and the fallback approval judge, run on.
    pub model: String,
}

impl AuxiliaryConfig {
    /// The connection the file's `small_model` entry names: its `vendor`
    /// resolved, and refused unless it speaks OpenAI's wire; its own key
    /// variable, else the vendor's.
    pub fn resolve(
        vendor: &str,
        base_url: Option<String>,
        api_key_env: Option<String>,
        model: String,
    ) -> Result<Self, String> {
        let vendor = Vendor::resolve(vendor)?;
        if vendor.wire() != Wire::OpenAi {
            return Err(format!(
                "vendor {:?} does not speak OpenAI's wire, which small_model's judge reads",
                vendor.name()
            ));
        }
        Ok(Self {
            vendor,
            base_url,
            api_key_env: vendor.api_key_env(api_key_env)?,
            model,
        })
    }
}

/// The generation options every call on this connection carries: reasoning
/// off, spelled by the vendor's dialect (`reasoning_effort: "none"` on
/// OpenAI's and Ollama's, `thinking: disabled` on DeepSeek's). A
/// reasoning-first model otherwise spends a small call's whole token budget
/// in its think block and never emits the answer: every stage-2 judge call
/// parsed as unparseable that way (8/8 escalations in the 2026-07-28
/// event-log investigation), and a title call cannot afford it either. A
/// model whose catalog row says it cannot turn reasoning off is sent the
/// call without the option rather than refused.
pub(crate) fn reasoning_off() -> GenerationOptions {
    GenerationOptions::default()
        .reasoning(Reasoning::Off)
        .on_unsupported(OnUnsupported::Ignore)
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
                let connection = (self.config.vendor)
                    .chat_connection(api_key, self.config.base_url.as_deref())
                    .ok()?;
                match connection {
                    ProviderConfig::OpenAi(config) => Some(config.client()),
                    // `resolve` admits OpenAI's wire alone.
                    _ => None,
                }
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
