//! The resolved `decision_model` connection.

/// The `decision_model` pair's openai-compatible entry and model id. Only
/// the secret's environment variable name crosses this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionConfig {
    /// The entry's `decision_url`: the full URL decisions are POSTed to,
    /// used as written.
    pub url: String,
    pub api_key_env: String,
    /// The decision model id (e.g. `clef:cloud`).
    pub model: String,
}
