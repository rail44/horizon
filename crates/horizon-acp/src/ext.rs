//! The `_horizon/*` extension methods: typed JSON-RPC requests and
//! notifications sent over the same connection as the standard ACP ones.

use std::path::PathBuf;

use agent_client_protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use horizon_wire::SessionId;

/// Reply of the extension requests that return nothing.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse,
)]
pub struct EmptyResponse {}

// -- client → agent requests --

/// Starts, or finds, a workspace's board organizer session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_horizon/ensure_board_organizer",
    response = EnsureBoardOrganizerResponse
)]
pub struct EnsureBoardOrganizerRequest {
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
pub struct EnsureBoardOrganizerResponse {
    pub session_id: SessionId,
}

// -- agent → client notifications --

/// Mirrors `contract::Event::Exited` (crates/horizon-agent/src/contract.rs)
/// and `hosting::AgentWireEvent::AttachmentClosed`
/// (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/session_event")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventNotification {
    /// The attachment ended: replaced, behind, detached, or the session is
    /// gone. ACP has no standard form for it.
    AttachmentClosed {
        session_id: SessionId,
        reason: AttachmentEnd,
    },
    /// The session's process-level exit. ACP has no standard form for it.
    Exited {
        session_id: SessionId,
        reason: String,
    },
}

/// Mirrors `hosting::AttachmentEnd` (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentEnd {
    Replaced,
    Lagged,
    Detached,
    SessionEnded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;
    use serde::de::DeserializeOwned;
    use std::fmt::Debug;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let encoded = serde_json::to_value(value).unwrap();
        assert!(encoded.is_object(), "params must be an object: {encoded}");
        let decoded: T = serde_json::from_value(encoded).unwrap();
        assert_eq!(&decoded, value);
    }

    fn message_round_trip<T: JsonRpcMessage + PartialEq + Debug>(value: &T) {
        let untyped = value.to_untyped_message().unwrap();
        let decoded = T::parse_message(untyped.method(), untyped.params()).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn requests_round_trip() {
        message_round_trip(&EnsureBoardOrganizerRequest {
            workspace_root: PathBuf::from("/work/project"),
        });
    }

    #[test]
    fn responses_round_trip() {
        round_trip(&EmptyResponse {});
        round_trip(&EnsureBoardOrganizerResponse {
            session_id: SessionId::new(),
        });
    }

    #[test]
    fn notifications_round_trip() {
        let session_id = SessionId::new();
        for event in [SessionEventNotification::Exited {
            session_id,
            reason: "terminated".into(),
        }]
        .into_iter()
        .chain(
            [
                AttachmentEnd::Replaced,
                AttachmentEnd::Lagged,
                AttachmentEnd::Detached,
                AttachmentEnd::SessionEnded,
            ]
            .map(|reason| SessionEventNotification::AttachmentClosed { session_id, reason }),
        ) {
            message_round_trip(&event);
        }
    }

    #[test]
    fn horizon_method_names_are_pinned() {
        let session_id = SessionId::new();
        let methods = [
            EnsureBoardOrganizerRequest {
                workspace_root: PathBuf::new(),
            }
            .method()
            .to_owned(),
            SessionEventNotification::Exited {
                session_id,
                reason: String::new(),
            }
            .method()
            .to_owned(),
        ];
        assert_eq!(
            methods,
            ["_horizon/ensure_board_organizer", "_horizon/session_event",]
        );
    }
}
