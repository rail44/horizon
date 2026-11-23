//! The vendor a `[[providers]]` entry is reached as: one of rig's provider
//! selections (`rig_core::providers::registry::ProviderId`). The vendor
//! decides the dialect a request is spelled in -- which generation options
//! a request may carry and how each is written for that provider -- and the
//! vendor the model catalog files the entry's models under.
//!
//! An endpoint rig has no dialect for is reached as OpenAI's vendor with the
//! entry's `base_url`: rig then spells requests as OpenAI's own and, for a
//! model OpenAI's catalog does not list, checks nothing about them, which is
//! what an OpenAI-compatible gateway needs (`docs/agent-reasoning-effort-design.md`).

use rig_core::providers::{
    openai,
    registry::{Format, ProviderConfig, ProviderId},
};

/// The wire family a vendor's requests travel on: the two Horizon runs
/// conversations over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wire {
    /// OpenAI's grammar, on its chat-completions endpoint.
    OpenAi,
    /// Anthropic's Messages grammar.
    Anthropic,
}

/// A resolved vendor: a selection rig can connect to, on a wire Horizon
/// runs conversations over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Vendor {
    id: ProviderId,
    wire: Wire,
}

impl Vendor {
    /// The vendor `name` names, as rig's catalog spells vendors
    /// (`"openai"`, `"anthropic"`, `"ollama"`, `"openrouter"`, ...). A vendor
    /// rig registers in two families is reached by its primary one.
    pub fn resolve(name: &str) -> Result<Self, String> {
        let id = ProviderId::catalog(name)
            .filter(ProviderId::is_registered)
            .ok_or_else(|| format!("vendor {name:?} is not a provider rig can connect to"))?;
        let wire = match id.format() {
            Some(Format::OpenAi) => Wire::OpenAi,
            Some(Format::Anthropic) => Wire::Anthropic,
            Some(format) => {
                return Err(format!(
                    "vendor {name:?} speaks {format}, which Horizon does not run conversations over"
                ));
            }
            None => {
                return Err(format!(
                    "vendor {name:?} is not a provider rig can connect to"
                ));
            }
        };
        Ok(Self { id, wire })
    }

    /// OpenAI's own vendor: the vendor an entry names when it names none.
    pub fn openai() -> Self {
        Self::resolve(super::DEFAULT_VENDOR).expect("rig registers OpenAI's own dialect")
    }

    /// The vendor's name, as rig's catalog spells it.
    pub fn name(&self) -> &'static str {
        self.id.vendor()
    }

    /// The wire family the vendor's requests travel on.
    pub fn wire(&self) -> Wire {
        self.wire
    }

    /// rig's selection, for the catalog's lookups.
    pub(crate) fn id(&self) -> ProviderId {
        self.id
    }

    /// The environment variable rig reads the vendor's key from, which an
    /// entry that names none of its own reads too.
    pub fn default_api_key_env(&self) -> Option<&'static str> {
        self.id.api_key_envs().into_iter().next()
    }

    /// The variable an entry's key is read from: the one it names, else
    /// the vendor's own ([`Self::default_api_key_env`]).
    pub fn api_key_env(&self, named: Option<String>) -> Result<String, String> {
        named
            .or_else(|| self.default_api_key_env().map(str::to_string))
            .ok_or_else(|| {
                format!(
                    "vendor {:?} has no key variable of its own; name one with api_key_env",
                    self.name()
                )
            })
    }

    /// The configuration requests go out on: the vendor's own preset with
    /// `api_key`, sent to `base_url` when the entry names one, on OpenAI's
    /// chat-completions endpoint for the OpenAI family (rig routes OpenAI's
    /// own dialect to `/responses`; every endpoint Horizon talks to speaks
    /// `/chat/completions`).
    pub(crate) fn connection(
        &self,
        api_key: String,
        base_url: Option<&str>,
    ) -> anyhow::Result<ProviderConfig> {
        let config = self
            .id
            .config(api_key)
            .ok_or_else(|| anyhow::anyhow!("rig cannot configure vendor {:?}", self.name()))?;
        let config = match config {
            ProviderConfig::OpenAi(config) => {
                ProviderConfig::OpenAi(config.with_route(openai::Route::Chat))
            }
            other => other,
        };
        Ok(match base_url {
            Some(base_url) => config.with_base_url(base_url),
            None => config,
        })
    }

    /// The endpoint the vendor's preset sends to, which an entry with no
    /// `base_url` of its own talks to.
    pub(crate) fn default_base_url(&self) -> String {
        self.id
            .config(String::new())
            .map(|config| config.base_url().to_string())
            .unwrap_or_default()
    }
}

impl Default for Vendor {
    fn default() -> Self {
        Self::openai()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rigs_vendors_resolve_to_their_wire_and_key_variable() {
        let openai = Vendor::resolve("openai").unwrap();
        assert_eq!(openai.wire(), Wire::OpenAi);
        assert_eq!(openai.default_api_key_env(), Some("OPENAI_API_KEY"));
        let anthropic = Vendor::resolve("anthropic").unwrap();
        assert_eq!(anthropic.wire(), Wire::Anthropic);
        assert_eq!(anthropic.default_api_key_env(), Some("ANTHROPIC_API_KEY"));
        // Ollama is registered in both families; its primary is OpenAI's.
        let ollama = Vendor::resolve("ollama").unwrap();
        assert_eq!(ollama.wire(), Wire::OpenAi);
        assert_eq!(ollama.name(), "ollama");
    }

    #[test]
    fn a_vendor_rig_cannot_reach_is_refused() {
        let unknown = Vendor::resolve("deepinfra").unwrap_err();
        assert!(unknown.contains("\"deepinfra\""), "{unknown}");
        // In the catalog, but served by a companion crate rather than rig.
        assert!(Vendor::resolve("aws_bedrock").is_err());
        // A wire Horizon runs no conversation over.
        assert!(Vendor::resolve("gemini").unwrap_err().contains("gemini"));
    }

    #[test]
    fn the_openai_family_goes_out_on_chat_completions_to_the_entrys_url() {
        let ProviderConfig::OpenAi(config) = Vendor::openai()
            .connection("key".into(), Some("https://gateway.invalid/v1"))
            .unwrap()
        else {
            panic!("OpenAI's vendor is OpenAI-shaped");
        };
        assert_eq!(config.completion_route(), openai::Route::Chat);
        assert_eq!(config.base_url, "https://gateway.invalid/v1");
    }

    #[test]
    fn an_entry_without_a_url_talks_to_the_vendors_own_endpoint() {
        assert_eq!(
            Vendor::openai().default_base_url(),
            "https://api.openai.com/v1"
        );
        let ollama = Vendor::resolve("ollama").unwrap();
        assert_eq!(
            ollama.connection("key".into(), None).unwrap().base_url(),
            ollama.default_base_url()
        );
    }
}
