use super::bounded::{NonEmpty, Number, Text};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigWrite {
    /// Complete replacement TOML document.
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadEntry {
    /// Entry id from the system prompt's skill or knowledge index.
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct KnowledgeWrite {
    /// Filename slug: lowercase letters, digits and hyphens.
    pub id: String,
    /// One-line summary shown in the project knowledge index.
    pub description: Text,
    /// Markdown body.
    pub body: String,
    /// Verifiable source references.
    pub sources: NonEmpty<String>,
    /// Omitted fields preserve existing entry values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchors: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::knowledge::KnowledgeStatus>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecallScope {
    #[default]
    Session,
    All,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnOutcome {
    Completed,
    Cancelled,
    Failed,
    Halted,
}

impl TurnOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Halted => "halted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecallSearch {
    /// FTS query. Supply query, turn_outcome, or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default)]
    pub scope: RecallScope,
    /// Defaults to this session. Cannot combine with scope=all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub limit: Number<1, 100, 20>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_outcome: Option<TurnOutcome>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecallRead {
    pub from_sequence: i64,
    /// Defaults to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub limit: Number<1, 100, 20>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Task {
    /// Short task label displayed in completion notifications.
    pub description: Text<200>,
    /// Self-contained question and exact deliverable for the child.
    pub prompt: Text<16384>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskOutput {
    /// Session id returned by task.
    pub session_id: uuid::Uuid,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebSearch {
    pub query: Text<2048>,
    #[serde(default)]
    pub num_results: Number<1, 10, 5>,
    #[serde(default)]
    pub max_characters: Number<1, 4000, 2000>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebFetch {
    /// Public HTTP(S) URL. Each redirect is checked against session grants.
    pub url: Text<8192>,
    #[serde(default)]
    pub max_characters: Number<1, 50000, 20000>,
}
