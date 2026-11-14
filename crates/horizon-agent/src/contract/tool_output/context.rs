use crate::contract::FoldedLogRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct ConfigRead {
    /// The configuration file that was looked for.
    pub path: String,
    /// Whether that file exists.
    pub exists: bool,
    /// The file's contents, when it exists.
    pub content: Option<String>,
    /// Why there is no content, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct SkillRead {
    /// The skill's id, which is its directory name.
    pub id: String,
    /// The one line the skill index shows for it.
    pub description: String,
    /// The skill's instructions.
    pub body: String,
    /// Whether the instructions were cut short.
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct KnowledgeRead {
    /// The entry's id.
    pub id: String,
    /// The one line the knowledge index shows for it.
    pub description: String,
    /// Files this entry is anchored to, used to notice when it goes stale.
    pub anchors: Vec<String>,
    /// Where the claims in it come from.
    pub sources: Vec<String>,
    /// When it was first written.
    pub created: String,
    /// When it was last updated.
    pub updated: String,
    /// Whether it is `active`, `needs-review`, or `expired`.
    pub status: String,
    /// The entry's full body.
    pub body: String,
    /// Whether the body was cut short.
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct KnowledgeWritten {
    /// The id it was written under.
    pub id: String,
    /// The one line the knowledge index will show for it.
    pub description: String,
    /// The file it was written to.
    pub path: String,
    /// When it was first written, unchanged by this write.
    pub created: String,
    /// When this write happened.
    pub updated: String,
    /// Its status after this write.
    pub status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallHit {
    /// The session the hit is in.
    pub session_id: String,
    /// Whether the hit is in the session doing the search.
    pub own_session: bool,
    /// The sequence to read around it with `recall_read`.
    pub sequence: i64,
    /// What kind of entry it is.
    pub kind: String,
    /// The role or tool that produced it.
    pub role_or_tool: String,
    /// The matching text.
    pub snippet: String,
    /// When it happened.
    pub at: String,
    /// For a tool result, whether it is an error.
    pub is_error: Option<bool>,
    /// How the turn it belongs to ended.
    pub turn_outcome: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallSearch {
    /// How many entries matched in total, which exceeds `hits` when capped.
    pub total: usize,
    /// The matches themselves.
    pub hits: Vec<RecallHit>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallEntry {
    /// The sequence number to pass back as `from_sequence`.
    pub sequence: i64,
    /// What kind of entry it is.
    pub kind: String,
    /// The role or tool that produced it.
    pub role_or_tool: String,
    /// The entry's text.
    pub text: String,
    /// When it happened.
    pub at: String,
    /// For a tool result, whether it is an error.
    pub is_error: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallRead {
    /// The entries in sequence order.
    pub entries: Vec<RecallEntry>,
    /// What the result could not carry, such as where the cap cut it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One session in the session-first history index. The aggregate labels keep
/// command failures, tool/runtime errors, and containment denials separate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallSession {
    /// The session's id, the key `recall_read` takes.
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
    /// The sequence to resume `recall_read` from.
    pub first_sequence: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RecallSessions {
    /// One entry per session, newest first.
    pub sessions: Vec<RecallSession>,
}
/// `worktree_enter`'s receipt: the activation is queued, not yet applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct WorktreeRequested {
    /// Whether the move was queued.
    pub requested: bool,
    /// The commit the new worktree branches from.
    pub base: String,
    /// What happens next, and when.
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
