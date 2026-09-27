//! Payloads carried under `_meta.horizon` on standard ACP messages, and the
//! helpers that put them in and take them out of a [`Meta`] map.

use std::path::PathBuf;

use agent_client_protocol::schema::v2::Meta;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use horizon_wire::SessionId;

/// The `_meta` key every Horizon payload lives under.
pub const HORIZON_META_KEY: &str = "horizon";

/// `stopReason` value for `contract::TurnEndReason::Failed`.
pub const STOP_REASON_FAILED: &str = "_horizon/failed";
/// `stopReason` value for `contract::TurnEndReason::HaltedByDoomLoop`.
pub const STOP_REASON_DOOM_LOOP: &str = "_horizon/doom_loop";

/// `configId` of the session config option that selects the provider and
/// model (`category: model`, `select`).
pub const MODEL_CONFIG_ID: &str = "model";

/// `optionId` of the approving choice on every `session/request_permission`.
pub const PERMISSION_OPTION_APPROVE: &str = "approve";
/// `optionId` of the denying choice on every `session/request_permission`.
pub const PERMISSION_OPTION_DENY: &str = "deny";

/// Carried under `_meta.horizon` on each `model` config option entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelOptionMeta {
    pub provider: String,
    pub model: String,
}

/// The `model` option's value id: `provider/model`, split at the first `/`
/// (provider names carry no `/`; model ids may).
pub fn encode_model_option_id(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Inverse of [`encode_model_option_id`].
pub fn decode_model_option_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('/')
}

/// Stores `value` under [`HORIZON_META_KEY`], creating the map if needed and
/// leaving every other key untouched.
pub fn write_horizon_meta<T: Serialize>(
    meta: &mut Option<Meta>,
    value: &T,
) -> Result<(), serde_json::Error> {
    let value = serde_json::to_value(value)?;
    meta.get_or_insert_with(Meta::new)
        .insert(HORIZON_META_KEY.to_owned(), value);
    Ok(())
}

/// Reads the payload under [`HORIZON_META_KEY`]: `None` when absent, the
/// decode result otherwise.
pub fn read_horizon_meta<T: DeserializeOwned>(
    meta: Option<&Meta>,
) -> Option<Result<T, serde_json::Error>> {
    let value = meta?.get(HORIZON_META_KEY)?;
    Some(T::deserialize(value))
}

/// `initialize` request and response, both directions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeMeta {
    pub ext_version: u32,
    pub binary_id: String,
}

/// Mirrors `wire::SessionNew` minus `workspace_root` (crates/horizon-agent/src/wire.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionNewMeta {
    pub session_id: SessionId,
    pub provider_id: String,
    pub role_id: Option<String>,
    pub isolate: bool,
    pub spawn_source_session_id: Option<SessionId>,
}

/// Mirrors `wire::SessionSummary` minus `session_id` (crates/horizon-agent/src/wire.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfoMeta {
    pub workspace_root: Option<PathBuf>,
    pub parent_session_id: Option<SessionId>,
    pub role_id: Option<String>,
    pub provider_id: String,
}

/// `tool_call_update`, whose `toolCallId` is the occurrence id. Mirrors
/// `contract::ToolCallRequest`/`ToolCallResult` (crates/horizon-agent/src/contract.rs)
/// and the result evidence fields (crates/horizon-agent/src/tools/output/evidence.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallMeta {
    pub call_id: String,
    pub tool_id: String,
    /// Present once the call has a result.
    #[serde(default)]
    pub outcome: Option<ToolOutcome>,
    #[serde(default)]
    pub auto_approved: Option<bool>,
    #[serde(default)]
    pub policy_tier: Option<String>,
    /// Present once a human has decided on the call's approval.
    #[serde(default)]
    pub human_decision: Option<HumanDecision>,
}

/// Mirrors `contract::ApprovalDecisionPayload` (crates/horizon-agent/src/contract.rs),
/// carried on the call's `tool_call_update` after `ApprovalResolved`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HumanDecision {
    Approved,
    Denied {
        #[serde(default)]
        reason: Option<String>,
    },
}

/// Mirrors `contract::ApprovalRequest` minus `reason`, keyed by its
/// `ToolCallIdentity` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalMeta {
    pub call_id: String,
    pub occurrence_id: String,
    pub kind: ApprovalKind,
}

/// Mirrors `contract::ApprovalKind` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalKind {
    Standard,
    DomainDenialRetry {
        domains: Vec<String>,
        prior_result: ToolResult,
    },
    FilesystemDenialRetry {
        denials: Vec<horizon_sandbox::FilesystemDenial>,
        grants: Vec<horizon_sandbox::FilesystemGrant>,
        prior_result: ToolResult,
    },
    DomainGrant {
        domains: Vec<String>,
    },
    GitOperation {
        writable_roots: Vec<PathBuf>,
    },
    MachServiceGrant {
        services: Vec<String>,
        prior_result: ToolResult,
    },
}

/// Mirrors `contract::ToolCallResult` (crates/horizon-agent/src/contract/tool_result.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolResult {
    pub call_id: String,
    pub occurrence_id: String,
    pub output: serde_json::Value,
    pub outcome: ToolOutcome,
}

/// Mirrors `contract::ToolOutcome` (crates/horizon-agent/src/contract/tool_result.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    Superseded { retry_occurrence_id: String },
}

/// Mirrors `contract::Command::DenyToolCall::reason` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionResponseMeta {
    pub reason: Option<String>,
}

/// Mirrors the non-human `contract::MessageRole` variants (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MessageMeta {
    pub role: MessageRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    TaskNotification,
    AutoContinue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fmt::Debug;

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

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let mut meta = None;
        write_horizon_meta(&mut meta, value).unwrap();
        let decoded: T = read_horizon_meta(meta.as_ref()).unwrap().unwrap();
        assert_eq!(&decoded, value);
    }

    fn prior_result() -> ToolResult {
        ToolResult {
            call_id: "call-1".into(),
            occurrence_id: "occ-1".into(),
            output: json!({"exit_code": 1}),
            outcome: ToolOutcome::Failed,
        }
    }

    #[test]
    fn meta_payloads_round_trip() {
        round_trip(&InitializeMeta {
            ext_version: crate::HORIZON_ACP_EXT_VERSION,
            binary_id: "horizon-agentd 0.1.0".into(),
        });
        round_trip(&SessionNewMeta {
            session_id: SessionId::new(),
            provider_id: "default".into(),
            role_id: Some("reviewer".into()),
            isolate: true,
            spawn_source_session_id: Some(SessionId::new()),
        });
        round_trip(&SessionInfoMeta {
            workspace_root: Some(PathBuf::from("/work/project/.horizon/worktrees/abcd")),
            parent_session_id: None,
            role_id: None,
            provider_id: "default".into(),
        });
        round_trip(&ToolCallMeta {
            call_id: "call-1".into(),
            tool_id: "bash".into(),
            outcome: None,
            auto_approved: None,
            policy_tier: None,
            human_decision: None,
        });
        round_trip(&ToolCallMeta {
            call_id: "call-1".into(),
            tool_id: "bash".into(),
            outcome: Some(ToolOutcome::Denied),
            auto_approved: Some(true),
            policy_tier: Some("contained".into()),
            human_decision: Some(HumanDecision::Denied {
                reason: Some("not now".into()),
            }),
        });
        round_trip(&ToolCallMeta {
            call_id: "call-2".into(),
            tool_id: "fs.edit".into(),
            outcome: Some(ToolOutcome::Succeeded),
            auto_approved: None,
            policy_tier: None,
            human_decision: Some(HumanDecision::Approved),
        });
        round_trip(&PermissionResponseMeta {
            reason: Some("not now".into()),
        });
        round_trip(&MessageMeta {
            role: MessageRole::TaskNotification,
        });
        round_trip(&MessageMeta {
            role: MessageRole::AutoContinue,
        });
    }

    #[test]
    fn every_approval_kind_round_trips() {
        let grant = horizon_sandbox::FilesystemGrant {
            path: PathBuf::from("/data"),
            access: horizon_sandbox::FilesystemGrantAccess::Read,
            scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: Vec::new(),
        };
        let kinds = [
            ApprovalKind::Standard,
            ApprovalKind::DomainDenialRetry {
                domains: vec!["example.com".into()],
                prior_result: prior_result(),
            },
            ApprovalKind::FilesystemDenialRetry {
                denials: vec![horizon_sandbox::FilesystemDenial {
                    attempted_path: PathBuf::from("/data/a.txt"),
                    grant: grant.clone(),
                }],
                grants: vec![grant],
                prior_result: prior_result(),
            },
            ApprovalKind::DomainGrant {
                domains: vec!["docs.example.com".into()],
            },
            ApprovalKind::GitOperation {
                writable_roots: vec![PathBuf::from("/work/project/.git")],
            },
            ApprovalKind::MachServiceGrant {
                services: vec!["com.apple.SecurityServer".into()],
                prior_result: ToolResult {
                    outcome: ToolOutcome::Superseded {
                        retry_occurrence_id: "occ-2".into(),
                    },
                    ..prior_result()
                },
            },
        ];
        for kind in kinds {
            round_trip(&ApprovalMeta {
                call_id: "call-1".into(),
                occurrence_id: "occ-1".into(),
                kind,
            });
        }
    }

    #[test]
    fn write_preserves_other_meta_keys() {
        let mut meta = Some(Meta::from_iter([
            ("other".to_owned(), json!({"keep": true})),
            (HORIZON_META_KEY.to_owned(), json!("stale")),
        ]));
        write_horizon_meta(&mut meta, &PermissionResponseMeta { reason: None }).unwrap();
        let meta = meta.unwrap();
        assert_eq!(meta.get("other"), Some(&json!({"keep": true})));
        assert_eq!(meta.get(HORIZON_META_KEY), Some(&json!({"reason": null})));
        assert_eq!(meta.len(), 2);
    }

    #[test]
    fn write_creates_the_map_when_absent() {
        let mut meta = None;
        write_horizon_meta(
            &mut meta,
            &MessageMeta {
                role: MessageRole::AutoContinue,
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::Value::Object(meta.unwrap()),
            json!({"horizon": {"role": "auto_continue"}})
        );
    }

    #[test]
    fn read_distinguishes_absent_from_malformed() {
        assert!(read_horizon_meta::<MessageMeta>(None).is_none());
        let without = Meta::from_iter([("other".to_owned(), json!(1))]);
        assert!(read_horizon_meta::<MessageMeta>(Some(&without)).is_none());
        let malformed = Meta::from_iter([(HORIZON_META_KEY.to_owned(), json!({"role": 7}))]);
        assert!(matches!(
            read_horizon_meta::<MessageMeta>(Some(&malformed)),
            Some(Err(_))
        ));
    }

    #[test]
    fn stop_reason_values_are_extension_prefixed() {
        assert_eq!(STOP_REASON_FAILED, "_horizon/failed");
        assert_eq!(STOP_REASON_DOOM_LOOP, "_horizon/doom_loop");
    }
}
