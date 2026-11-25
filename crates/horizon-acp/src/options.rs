//! The ids Horizon's agent and shell agree on inside standard ACP: the
//! session config options' ids and values, and the permission options'.

/// `configId` of the session config option that selects the provider and
/// model (`category: model`, `select`).
pub const MODEL_CONFIG_ID: &str = "model";

/// `configId` of the session config option that selects the reasoning
/// effort (`category: thought_level`, `select`): each entry's value id is
/// the level's word (`"none"`, `"minimal"`, ... `"max"`), and the entries
/// are the levels the session's current model takes.
pub const EFFORT_CONFIG_ID: &str = "effort";

/// `optionId` of the approving choice on every `session/request_permission`.
pub const PERMISSION_OPTION_APPROVE: &str = "approve";
/// `optionId` of the denying choice on every `session/request_permission`.
pub const PERMISSION_OPTION_DENY: &str = "deny";

/// The `model` option's value id: `provider/model`, split at the first `/`
/// (provider names carry no `/`; model ids may).
pub fn encode_model_option_id(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Inverse of [`encode_model_option_id`].
pub fn decode_model_option_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_option_id_round_trips_and_splits_at_the_first_slash() {
        let id = encode_model_option_id("openrouter", "anthropic/claude-x");
        assert_eq!(id, "openrouter/anthropic/claude-x");
        assert_eq!(
            decode_model_option_id(&id),
            Some(("openrouter", "anthropic/claude-x"))
        );
        assert_eq!(decode_model_option_id("no-slash"), None);
    }
}
