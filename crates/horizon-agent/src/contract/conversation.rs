//! Durable provider conversation changes, distinct from transcript display events.
use super::{JsonValue, ToolCallIdentity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ConversationInputKind {
    User,
    Notification,
    Continuation,
}

/// The message codec is explicit because opaque provider fields must survive
/// without being interpreted by the transcript or the persistence layer.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ConversationRecord {
    TurnOpened,
    Input {
        kind: ConversationInputKind,
        text: String,
    },
    ToolAnnounced {
        response_id: String,
        identity: ToolCallIdentity,
        codec: u32,
        tool_call: JsonValue,
        reasoning: JsonValue,
    },
    Response {
        response_id: String,
        codec: u32,
        message: JsonValue,
        calls: Vec<ToolCallIdentity>,
    },
}
