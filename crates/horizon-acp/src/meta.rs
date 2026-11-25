//! Payloads carried under `_meta.horizon` on standard ACP messages, and the
//! helpers that put them in and take them out of a [`Meta`] map.

use std::path::PathBuf;

use agent_client_protocol::v2::schema::Meta;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use horizon_wire::SessionId;

/// The `_meta` key every Horizon payload lives under.
pub const HORIZON_META_KEY: &str = "horizon";

/// `configId` of the session config option that selects the role a session
/// starts with (`category: mode`, `select`): a role's id, or
/// [`DEFAULT_MODE`] for none. It is set when the session starts.
pub const MODE_CONFIG_ID: &str = "mode";

/// The `mode` option's value for a session with no role.
pub const DEFAULT_MODE: &str = "default";

/// `configId` of the session config option that selects the provider and
/// model (`category: model`, `select`).
pub const MODEL_CONFIG_ID: &str = "model";

/// `configId` of the session config option that selects the reasoning
/// effort (`category: thought_level`, `select`): each entry's value id is
/// the level's word (`"none"`, `"minimal"`, ... `"max"`), and the entries
/// are the levels the session's current model takes.
pub const EFFORT_CONFIG_ID: &str = "effort";

/// `optionId` of the approving choice on every `session/request_permission`.
pub const PERMISSION_OPTION_APPROVE: &str = "approve";
/// `optionId` of the denying choice on every `session/request_permission`.
pub const PERMISSION_OPTION_DENY: &str = "deny";

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

/// Mirrors `hosting::SessionNew`'s id and spawn source (crates/horizon-agent/src/hosting.rs):
/// its role and model are the `mode` and `model` options' initial values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionNewMeta {
    pub session_id: SessionId,
    pub spawn_source_session_id: Option<SessionId>,
}

/// Mirrors `hosting::SessionSummary` minus `session_id` (crates/horizon-agent/src/hosting.rs).
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
}

/// Mirrors `contract::ApprovalRequest` minus `reason`, keyed by its
/// `ToolCallIdentity` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalMeta {
    pub call_id: String,
    pub occurrence_id: String,
    pub kind: ApprovalKind,
    /// The `task` child this approval is for, when it is not the session's
    /// own: shown through the session, answered to the child
    /// (`docs/agent-subagent-design.md` decision 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_session_id: Option<String>,
}

/// Mirrors `contract::ApprovalKind` (crates/horizon-agent/src/contract.rs).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalKind {
    Standard,
    DomainGrant { domains: Vec<String> },
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

    #[test]
    fn meta_payloads_round_trip() {
        round_trip(&InitializeMeta {
            ext_version: crate::HORIZON_ACP_EXT_VERSION,
            binary_id: "horizon-agentd 0.1.0".into(),
        });
        round_trip(&SessionNewMeta {
            session_id: SessionId::new(),
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
        });
        round_trip(&ToolCallMeta {
            call_id: "call-1".into(),
            tool_id: "bash".into(),
            outcome: Some(ToolOutcome::Denied),
            auto_approved: Some(true),
            policy_tier: Some("contained".into()),
        });
        round_trip(&ToolCallMeta {
            call_id: "call-2".into(),
            tool_id: "fs_edit".into(),
            outcome: Some(ToolOutcome::Succeeded),
            auto_approved: None,
            policy_tier: None,
        });
    }

    #[test]
    fn every_approval_kind_round_trips() {
        let kinds = [
            ApprovalKind::Standard,
            ApprovalKind::DomainGrant {
                domains: vec!["docs.example.com".into()],
            },
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            round_trip(&ApprovalMeta {
                call_id: "call-1".into(),
                occurrence_id: "occ-1".into(),
                kind,
                task_session_id: (index % 2 == 1).then(|| "task-1".into()),
            });
        }
    }

    /// A payload for the helpers' own tests.
    fn probe() -> InitializeMeta {
        InitializeMeta {
            ext_version: 1,
            binary_id: "probe".into(),
        }
    }

    #[test]
    fn write_preserves_other_meta_keys() {
        let mut meta = Some(Meta::from_iter([
            ("other".to_owned(), json!({"keep": true})),
            (HORIZON_META_KEY.to_owned(), json!("stale")),
        ]));
        write_horizon_meta(&mut meta, &probe()).unwrap();
        let meta = meta.unwrap();
        assert_eq!(meta.get("other"), Some(&json!({"keep": true})));
        assert_eq!(
            meta.get(HORIZON_META_KEY),
            Some(&json!({"ext_version": 1, "binary_id": "probe"}))
        );
        assert_eq!(meta.len(), 2);
    }

    #[test]
    fn write_creates_the_map_when_absent() {
        let mut meta = None;
        write_horizon_meta(&mut meta, &probe()).unwrap();
        assert_eq!(
            serde_json::Value::Object(meta.unwrap()),
            json!({"horizon": {"ext_version": 1, "binary_id": "probe"}})
        );
    }

    #[test]
    fn read_distinguishes_absent_from_malformed() {
        assert!(read_horizon_meta::<InitializeMeta>(None).is_none());
        let without = Meta::from_iter([("other".to_owned(), json!(1))]);
        assert!(read_horizon_meta::<InitializeMeta>(Some(&without)).is_none());
        let malformed =
            Meta::from_iter([(HORIZON_META_KEY.to_owned(), json!({"ext_version": "x"}))]);
        std::assert_matches!(
            read_horizon_meta::<InitializeMeta>(Some(&malformed)),
            Some(Err(_))
        );
    }
}
