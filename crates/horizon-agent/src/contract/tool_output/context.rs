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

/// One session in the session-first history index. The aggregate labels keep
/// command failures, tool/runtime errors, and containment denials separate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallSession {
    /// The session's id, the key `recall.read` takes.
    pub session_id: String,
    /// When the session was most recently active.
    pub at: String,
    /// The first user request that opened the session.
    pub first_request: Option<String>,
    /// The most recent user request.
    pub latest_request: Option<String>,
    /// How the last turn ended, when it produced an outcome.
    pub last_turn_outcome: Option<String>,
    /// How many turns the session ran.
    pub turn_count: usize,
    /// Tool results that failed without a command exit code and without a
    /// containment refusal.
    pub tool_error_count: usize,
    /// Command results whose exit code was non-zero.
    pub command_failure_count: usize,
    /// Tool results carrying sandbox containment refusals -- filesystem,
    /// network, mach service, or ungrantable paths. A refused call that was
    /// then retried under approval counts here once.
    pub denial_count: usize,
    /// Approval requests raised in this session, whether a human answered
    /// them, denied them, or left them unanswered.
    pub approval_count: usize,
    /// The sequence to resume `recall.read` from.
    pub first_sequence: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RecallSessions {
    pub sessions: Vec<RecallSession>,
}
/// `worktree.enter`'s receipt: the activation is queued, not yet applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorktreeRequested {
    pub requested: bool,
    pub base: String,
    pub message: String,
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
