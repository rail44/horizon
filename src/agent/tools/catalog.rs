use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::contract::ToolPermission;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Definition {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) permission: ToolPermission,
}

pub(crate) fn definitions() -> Vec<Definition> {
    vec![
        Definition {
            id: "workspace.snapshot".to_string(),
            title: "Workspace Snapshot".to_string(),
            description: "Read tabs, panes, sessions, and active workspace state.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            permission: ToolPermission::AutoAllowRead,
        },
        Definition {
            id: "mock.approval_required".to_string(),
            title: "Mock Approval Required".to_string(),
            description: "Test tool that exercises the approval flow.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            permission: ToolPermission::RequireApproval,
        },
    ]
}

pub(crate) fn permission_for_tool(tool_id: &str) -> Option<ToolPermission> {
    definitions()
        .into_iter()
        .find(|definition| definition.id == tool_id)
        .map(|definition| definition.permission)
}
