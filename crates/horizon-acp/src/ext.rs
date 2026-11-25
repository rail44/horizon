//! The `_horizon/*` extension methods: typed JSON-RPC notifications sent
//! over the same connection as the standard ACP ones.

use agent_client_protocol::JsonRpcNotification;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use horizon_wire::SessionId;

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
    use std::fmt::Debug;

    fn message_round_trip<T: JsonRpcMessage + PartialEq + Debug>(value: &T) {
        let untyped = value.to_untyped_message().unwrap();
        let decoded = T::parse_message(untyped.method(), untyped.params()).unwrap();
        assert_eq!(&decoded, value);
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
        let methods = [SessionEventNotification::Exited {
            session_id,
            reason: String::new(),
        }
        .method()
        .to_owned()];
        assert_eq!(methods, ["_horizon/session_event"]);
    }
}
