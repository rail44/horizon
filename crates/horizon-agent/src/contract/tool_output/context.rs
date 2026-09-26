use crate::contract::FoldedLogRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfigRead {
    pub path: String,
    pub exists: bool,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SkillRead {
    pub id: String,
    pub description: String,
    pub body: String,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct KnowledgeRead {
    pub id: String,
    pub description: String,
    pub anchors: Vec<String>,
    pub sources: Vec<String>,
    pub created: String,
    pub updated: String,
    pub status: String,
    pub body: String,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct KnowledgeWritten {
    pub id: String,
    pub description: String,
    pub path: String,
    pub created: String,
    pub updated: String,
    pub status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecallHit {
    pub session_id: String,
    pub own_session: bool,
    pub sequence: i64,
    pub kind: String,
    pub role_or_tool: String,
    pub snippet: String,
    pub at: String,
    pub is_error: Option<bool>,
    pub turn_outcome: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecallSearch {
    pub total: usize,
    pub hits: Vec<RecallHit>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecallEntry {
    pub sequence: i64,
    pub kind: String,
    pub role_or_tool: String,
    pub text: String,
    pub at: String,
    pub is_error: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecallRead {
    pub entries: Vec<RecallEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MemoryUpdated {
    Skipped {
        ok: bool,
        no_update: bool,
        reason: String,
    },
    Updated {
        ok: bool,
        fields_updated: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        folded_log_range: Option<FoldedLogRange>,
    },
}
