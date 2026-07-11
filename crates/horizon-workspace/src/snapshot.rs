//! The `workspace.snapshot` host-tool payload: the JSON view of the
//! workspace an agent session sees when it asks its host. Shared by both
//! shells (each answers the wire request with this same shape).

use serde_json::json;

use crate::types::Workspace;

pub fn workspace_snapshot(workspace: &Workspace) -> serde_json::Value {
    json!({
        "tab_count": workspace.tab_count(),
        "detached_session_count": workspace.detached_session_count(),
        "active_title": workspace.active_title(),
        "active_visible_index": workspace.active_visible_index(),
        "tabs": workspace
            .tab_summaries()
            .into_iter()
            .map(|tab| json!({
                "index": tab.index,
                "title": tab.title,
                "active": tab.active,
                "pane_count": tab.pane_count,
                "active_session_id": tab.active_session_id.map(|id| format!("{id:?}")),
            }))
            .collect::<Vec<_>>(),
        "panes": workspace
            .pane_summaries()
            .into_iter()
            .map(|pane| json!({
                "tab_index": pane.tab_index,
                "pane_index": pane.pane_index,
                "title": pane.title,
                "kind": format!("{:?}", pane.kind).to_ascii_lowercase(),
                "active": pane.active,
                "tab_active": pane.tab_active,
            }))
            .collect::<Vec<_>>(),
        "sessions": workspace
            .session_summaries()
            .into_iter()
            .map(|session| json!({
                "id": format!("{:?}", session.id),
                "kind": format!("{:?}", session.kind).to_ascii_lowercase(),
                "display_number": session.display_number,
                "title": session.title,
                "attached": session.attached,
            }))
            .collect::<Vec<_>>(),
    })
}
