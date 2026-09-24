//! Tool execution outcomes. Payload fields are tool data; lifecycle meaning
//! belongs to the result envelope and survives persistence and replay.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{JsonValue, OccurrenceId, ToolCallId};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    Superseded { retry_occurrence_id: OccurrenceId },
}

impl ToolOutcome {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Failed | Self::Denied)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub occurrence_id: OccurrenceId,
    pub output: JsonValue,
    pub outcome: ToolOutcome,
}

impl ToolCallResult {
    /// Normalize a tool handler's success/failure convention once at the
    /// execution boundary. Cancellation, user denial, and retry replacement
    /// have explicit constructors; payload text never implies those states.
    pub fn new(
        call_id: ToolCallId,
        occurrence_id: OccurrenceId,
        output: impl Into<JsonValue>,
    ) -> Self {
        let output = output.into();
        let outcome = if output.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        };
        Self {
            call_id,
            occurrence_id,
            output,
            outcome,
        }
    }

    pub fn is_error(&self) -> bool {
        self.outcome.is_error()
    }
    pub fn is_denied(&self) -> bool {
        self.outcome == ToolOutcome::Denied
    }
    pub fn is_superseded(&self) -> bool {
        matches!(self.outcome, ToolOutcome::Superseded { .. })
    }

    pub(crate) fn denied(
        call_id: ToolCallId,
        occurrence_id: OccurrenceId,
        output: impl Into<JsonValue>,
    ) -> Self {
        Self {
            outcome: ToolOutcome::Denied,
            ..Self::new(call_id, occurrence_id, output)
        }
    }

    pub fn cancelled(identity: super::ToolCallIdentity) -> Self {
        Self {
            call_id: identity.call_id,
            occurrence_id: identity.occurrence_id,
            outcome: ToolOutcome::Cancelled,
            output: serde_json::json!({ "cancelled": true }).into(),
        }
    }

    /// Retire an attempt without answering the provider's pending call. The
    /// replacement attempt will supply that answer, identified explicitly here.
    pub fn superseded_by_retry(&self, retry_occurrence: &OccurrenceId) -> Self {
        Self {
            call_id: self.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
            outcome: ToolOutcome::Superseded { retry_occurrence_id: retry_occurrence.clone() },
            output: serde_json::json!({"message": "this attempt ended; a new attempt of the same call replaces it"}).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_markers_cannot_impersonate_lifecycle_outcomes() {
        for output in [
            json!({"cancelled": true}),
            json!({"superseded_by_retry": true}),
            json!({"message": "denied by user"}),
        ] {
            let result =
                ToolCallResult::new(ToolCallId("call".into()), OccurrenceId::new(), output);
            assert_eq!(result.outcome, ToolOutcome::Succeeded);
        }
    }

    #[test]
    fn current_format_rejects_legacy_flags_and_missing_outcome() {
        let result = ToolCallResult::new(ToolCallId("call".into()), OccurrenceId::new(), json!({}));
        for key in ["denied", "is_error", "outcome"] {
            let mut encoded = serde_json::to_value(&result).unwrap();
            if key == "outcome" {
                encoded.as_object_mut().unwrap().remove(key);
            } else {
                encoded[key] = json!(false);
            }
            assert!(serde_json::from_value::<ToolCallResult>(encoded).is_err());
        }
    }
}
