//! The resolved `decision_model` connection.

/// The `decision_model` pair's openai-compatible entry and model id. Only
/// the secret's environment variable name crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionConfig {
    pub base_url: Option<String>,
    pub api_key_env: String,
    /// The decision model id (e.g. `clef:cloud`).
    pub model: String,
}

impl DecisionConfig {
    /// Applies the same `OPENAI_BASE_URL` override as the other
    /// openai-compatible connections; the file layer validated the entry's
    /// kind.
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
