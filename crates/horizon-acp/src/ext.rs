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

/// Mirrors `contract::Command::ContinueTurn` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_horizon/continue_turn", response = EmptyResponse)]
pub struct ContinueTurnRequest {
    pub session_id: SessionId,
}

/// Lists the configured providers.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest,
)]
#[request(method = "_horizon/list_providers", response = ListProvidersResponse)]
pub struct ListProvidersRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
pub struct ListProvidersResponse {
    pub providers: Vec<ProviderSummary>,
}

/// Mirrors `hosting::ProviderSummary` (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderSummary {
    pub name: String,
    pub base_url: Option<String>,
    /// The environment variable name the entry's API key is read from.
    pub api_key_env: String,
    pub default_model: Option<String>,
    pub available: bool,
    pub default: bool,
}

/// Lists a provider's own model ids (its `GET /models`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_horizon/list_provider_models",
    response = ListProviderModelsResponse
)]
pub struct ListProviderModelsRequest {
    pub provider: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
pub struct ListProviderModelsResponse {
    pub models: Vec<String>,
}

/// Starts watching a workspace's board.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_horizon/watch_board", response = EmptyResponse)]
pub struct WatchBoardRequest {
    pub workspace_root: PathBuf,
}

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

/// Reloads the provider configuration from the config file.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest,
)]
#[request(method = "_horizon/reload_provider_config", response = EmptyResponse)]
pub struct ReloadProviderConfigRequest {}

/// Flushes the event log and exits the daemon.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest,
)]
#[request(method = "_horizon/drain", response = EmptyResponse)]
pub struct DrainRequest {}

// -- agent → client request --

/// Mirrors `hosting::HostToolRequest` (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_horizon/host_tool", response = HostToolResponse)]
pub struct HostToolRequest {
    pub request_id: String,
    pub tool_id: String,
    pub input: serde_json::Value,
}

/// Mirrors `hosting::HostToolResponse` (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
pub struct HostToolResponse {
    pub output: serde_json::Value,
}

// -- agent → client notifications --

/// Mirrors `contract::TaskProgress` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/task_progress")]
pub struct TaskProgressNotification {
    /// The requesting session the progress row belongs to.
    pub session_id: SessionId,
    pub task_session_id: SessionId,
    pub description: String,
    pub state: TaskProgressState,
    pub activity: Option<String>,
    pub started_at_epoch_ms: u64,
}

/// Mirrors `contract::TaskProgressState` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskProgressState {
    Running,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/tool_call_progress")]
pub struct ToolCallProgressNotification {
    pub session_id: SessionId,
    #[serde(flatten)]
    pub event: ToolCallProgressEvent,
}

/// Mirrors `hosting::AgentWireEvent::{ToolCallProgress, ToolCallProgressClosed}`
/// (crates/horizon-agent/src/hosting.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallProgressEvent {
    /// Mirrors `contract::ToolCallProgress` (crates/horizon-agent/src/contract.rs).
    Progress {
        key: String,
        #[serde(default)]
        tool_id: Option<String>,
        bytes: usize,
    },
    Closed {
        key: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/memory")]
pub struct MemoryNotification {
    pub session_id: SessionId,
    #[serde(flatten)]
    pub event: MemoryEvent,
}

/// Mirrors `contract::Event::{MemoryDigest, MemoryCheckpointMissed}`
/// (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryEvent {
    Digest(MemoryDigest),
    CheckpointMissed,
}

/// Mirrors `contract::MemoryDigest` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryDigest {
    pub updates: Vec<MemoryFieldUpdate>,
    pub folded_log_range: Option<FoldedLogRange>,
    pub no_update_reason: Option<String>,
}

/// Mirrors `contract::MemoryFieldUpdate` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryFieldUpdate {
    pub field: MemoryField,
    pub op: MemoryOp,
    pub content: String,
}

/// Mirrors `contract::MemoryField` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryField {
    Goal,
    Decisions,
    Completed,
    InProgress,
    Stuck,
    NextStep,
    Related,
}

/// Mirrors `contract::MemoryOp` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOp {
    Set,
    Append,
    Clear,
}

/// Mirrors `contract::FoldedLogRange` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FoldedLogRange {
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Mirrors `contract::Event::{SessionResumed, ProviderRateLimited,
/// HistoryCleared, Error, Exited}` (crates/horizon-agent/src/contract.rs),
/// `hosting::AgentWireEvent::AttachmentClosed`
/// (crates/horizon-agent/src/hosting.rs) and the daemon's startup
/// event-log skipped-lines diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/session_event")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventNotification {
    SessionResumed {
        session_id: SessionId,
    },
    ProviderRateLimited {
        session_id: SessionId,
        status: Option<u16>,
        attempt: u32,
        backoff_ms: u64,
    },
    HistoryCleared {
        session_id: SessionId,
        cleared_occurrence_ids: Vec<String>,
        recovered_chars: u64,
    },
    AttachmentClosed {
        session_id: SessionId,
        reason: AttachmentEnd,
    },
    /// A mid-session failure the daemon reports outside any request.
    Error {
        session_id: SessionId,
        message: String,
    },
    /// The session's process-level exit.
    Exited {
        session_id: SessionId,
        reason: String,
    },
    /// The daemon's startup event-log corruption summary; not per-session.
    SkippedLines {
        summary: String,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcNotification)]
#[notification(method = "_horizon/provider_request")]
pub struct ProviderRequestNotification {
    pub session_id: SessionId,
    #[serde(flatten)]
    pub event: ProviderRequestEvent,
}

/// Mirrors `contract::Event::{ProviderRequestSent, ProviderRequestFirstToken,
/// ProviderRequestFinished}` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderRequestEvent {
    Sent { model: String },
    FirstToken,
    Finished,
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

    fn provider_summary() -> ProviderSummary {
        ProviderSummary {
            name: "default".into(),
            base_url: Some("https://api.example.com/v1".into()),
            api_key_env: "OPENAI_API_KEY".into(),
            default_model: None,
            available: true,
            default: true,
        }
    }

    #[test]
    fn requests_round_trip() {
        let session_id = SessionId::new();
        message_round_trip(&ContinueTurnRequest { session_id });
        message_round_trip(&ListProvidersRequest {});
        message_round_trip(&ListProviderModelsRequest {
            provider: "default".into(),
        });
        message_round_trip(&WatchBoardRequest {
            workspace_root: PathBuf::from("/work/project"),
        });
        message_round_trip(&EnsureBoardOrganizerRequest {
            workspace_root: PathBuf::from("/work/project"),
        });
        message_round_trip(&ReloadProviderConfigRequest {});
        message_round_trip(&DrainRequest {});
        message_round_trip(&HostToolRequest {
            request_id: "req-1".into(),
            tool_id: "workspace.snapshot".into(),
            input: serde_json::json!({"nested": [1, {"k": true}]}),
        });
    }

    #[test]
    fn responses_round_trip() {
        round_trip(&EmptyResponse {});
        round_trip(&ListProvidersResponse {
            providers: vec![provider_summary()],
        });
        round_trip(&ListProviderModelsResponse {
            models: vec!["gpt-4o".into(), "o3".into()],
        });
        round_trip(&EnsureBoardOrganizerResponse {
            session_id: SessionId::new(),
        });
        round_trip(&HostToolResponse {
            output: serde_json::json!({"tabs": []}),
        });
    }

    #[test]
    fn notifications_round_trip() {
        let session_id = SessionId::new();
        message_round_trip(&TaskProgressNotification {
            session_id,
            task_session_id: SessionId::new(),
            description: "scan".into(),
            state: TaskProgressState::Running,
            activity: Some("fs.grep".into()),
            started_at_epoch_ms: 1_700_000_000_000,
        });
        for event in [
            ToolCallProgressEvent::Progress {
                key: "call-1".into(),
                tool_id: Some("fs.edit".into()),
                bytes: 64,
            },
            ToolCallProgressEvent::Progress {
                key: "call-1".into(),
                tool_id: None,
                bytes: 0,
            },
            ToolCallProgressEvent::Closed {
                key: "call-1".into(),
            },
        ] {
            message_round_trip(&ToolCallProgressNotification { session_id, event });
        }
        message_round_trip(&MemoryNotification {
            session_id,
            event: MemoryEvent::Digest(MemoryDigest {
                updates: vec![MemoryFieldUpdate {
                    field: MemoryField::NextStep,
                    op: MemoryOp::Append,
                    content: "ship it".into(),
                }],
                folded_log_range: Some(FoldedLogRange {
                    from_seq: 3,
                    to_seq: 9,
                }),
                no_update_reason: None,
            }),
        });
        message_round_trip(&MemoryNotification {
            session_id,
            event: MemoryEvent::CheckpointMissed,
        });
        for event in [
            SessionEventNotification::SessionResumed { session_id },
            SessionEventNotification::ProviderRateLimited {
                session_id,
                status: Some(429),
                attempt: 2,
                backoff_ms: 1500,
            },
            SessionEventNotification::HistoryCleared {
                session_id,
                cleared_occurrence_ids: vec!["occ-1".into(), "occ-2".into()],
                recovered_chars: 4096,
            },
            SessionEventNotification::Error {
                session_id,
                message: "provider unreachable".into(),
            },
            SessionEventNotification::Exited {
                session_id,
                reason: "terminated".into(),
            },
            SessionEventNotification::SkippedLines {
                summary: "2 lines skipped".into(),
            },
        ]
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
        for event in [
            ProviderRequestEvent::Sent {
                model: "gpt-4o".into(),
            },
            ProviderRequestEvent::FirstToken,
            ProviderRequestEvent::Finished,
        ] {
            message_round_trip(&ProviderRequestNotification { session_id, event });
        }
    }

    #[test]
    fn flattened_notifications_keep_session_id_beside_the_tag() {
        let session_id = SessionId::new();
        let encoded = serde_json::to_value(ProviderRequestNotification {
            session_id,
            event: ProviderRequestEvent::Sent {
                model: "gpt-4o".into(),
            },
        })
        .unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "session_id": session_id,
                "type": "sent",
                "model": "gpt-4o",
            })
        );
    }

    #[test]
    fn horizon_method_names_are_pinned() {
        let session_id = SessionId::new();
        let methods = [
            ContinueTurnRequest { session_id }.method().to_owned(),
            ListProvidersRequest {}.method().to_owned(),
            ListProviderModelsRequest {
                provider: String::new(),
            }
            .method()
            .to_owned(),
            WatchBoardRequest {
                workspace_root: PathBuf::new(),
            }
            .method()
            .to_owned(),
            EnsureBoardOrganizerRequest {
                workspace_root: PathBuf::new(),
            }
            .method()
            .to_owned(),
            ReloadProviderConfigRequest {}.method().to_owned(),
            DrainRequest {}.method().to_owned(),
            HostToolRequest {
                request_id: String::new(),
                tool_id: String::new(),
                input: serde_json::Value::Null,
            }
            .method()
            .to_owned(),
            TaskProgressNotification {
                session_id,
                task_session_id: session_id,
                description: String::new(),
                state: TaskProgressState::Finished,
                activity: None,
                started_at_epoch_ms: 0,
            }
            .method()
            .to_owned(),
            ToolCallProgressNotification {
                session_id,
                event: ToolCallProgressEvent::Closed { key: String::new() },
            }
            .method()
            .to_owned(),
            MemoryNotification {
                session_id,
                event: MemoryEvent::CheckpointMissed,
            }
            .method()
            .to_owned(),
            SessionEventNotification::SessionResumed { session_id }
                .method()
                .to_owned(),
            ProviderRequestNotification {
                session_id,
                event: ProviderRequestEvent::Finished,
            }
            .method()
            .to_owned(),
        ];
        assert_eq!(
            methods,
            [
                "_horizon/continue_turn",
                "_horizon/list_providers",
                "_horizon/list_provider_models",
                "_horizon/watch_board",
                "_horizon/ensure_board_organizer",
                "_horizon/reload_provider_config",
                "_horizon/drain",
                "_horizon/host_tool",
                "_horizon/task_progress",
                "_horizon/tool_call_progress",
                "_horizon/memory",
                "_horizon/session_event",
                "_horizon/provider_request",
            ]
        );
    }
}
