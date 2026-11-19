//! The `workspace_snapshot` host-tool payload: the JSON view of the
//! workspace an agent session sees when it asks its host. Shared by both
//! shells (each answers the wire request with this same shape).
//!
//! The layout is the workspace's tree as it is
//! (`docs/workspace-layout-design.md`): a tab is named by its group --
//! the group's place among the tree's groups, outermost first -- and its
//! index in that group.

use serde_json::{Value, json};

use crate::types::{LayoutNode, SplitAxis, Workspace};

pub fn workspace_snapshot(workspace: &Workspace) -> Value {
    let groups: Vec<_> = (workspace.layout().groups().into_iter())
        .map(|group| group.id)
        .collect();
    json!({
        "detached_session_count": workspace.detached_session_count(),
        "active_title": workspace.active_title(),
        "layout": layout_of(workspace, workspace.layout(), &groups),
        "sessions": workspace
            .session_summaries()
            .into_iter()
            .map(|session| json!({
                "id": format!("{:?}", session.id),
                "kind": session.kind.label(),
                "display_number": session.display_number,
                "title": session.title,
                "attached": session.attached,
            }))
            .collect::<Vec<_>>(),
    })
}

fn layout_of(
    workspace: &Workspace,
    node: &LayoutNode,
    groups: &[crate::types::TabGroupId],
) -> Value {
    match node {
        LayoutNode::Pane(pane_id) => {
            let pane = workspace.panes.iter().find(|pane| pane.id == *pane_id);
            json!({
                "pane": {
                    "title": pane.map(|pane| workspace.pane_title(pane)),
                    "kind": pane.map(|pane| pane.plugin.name.clone()),
                    "session_id": workspace.pane_session_id(*pane_id).map(|id| format!("{id:?}")),
                    "focused": workspace.is_active_pane(*pane_id),
                }
            })
        }
        LayoutNode::Split { axis, children } => json!({
            "split": {
                "axis": match axis {
                    SplitAxis::Horizontal => "horizontal",
                    SplitAxis::Vertical => "vertical",
                },
                "children": children
                    .iter()
                    .map(|child| json!({
                        "weight": child.weight,
                        "node": layout_of(workspace, &child.node, groups),
                    }))
                    .collect::<Vec<_>>(),
            }
        }),
        LayoutNode::Tabs(group) => json!({
            "tabs": {
                "group": groups.iter().position(|id| *id == group.id),
                "tabs": group
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(index, tab)| json!({
                        "index": index,
                        "title": workspace.tab_title(tab),
                        "shown": tab.id == group.shown,
                        "layout": layout_of(workspace, &tab.root, groups),
                    }))
                    .collect::<Vec<_>>(),
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionId, SessionKind};

    #[test]
    fn the_snapshot_gives_the_tree_and_names_a_tab_by_its_group_and_index() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        workspace.open_tab_with_new_session_activated(SessionKind::Agent, true);

        let snapshot = workspace_snapshot(&workspace);
        let tabs = &snapshot["layout"]["tabs"];
        assert_eq!(tabs["group"], 0);
        assert_eq!(tabs["tabs"][0]["index"], 0);
        assert_eq!(tabs["tabs"][0]["shown"], false);
        assert_eq!(tabs["tabs"][1]["shown"], true);
        let split = &tabs["tabs"][0]["layout"]["split"];
        assert_eq!(split["axis"], "horizontal");
        assert_eq!(split["children"][0]["node"]["pane"]["title"], "Terminal #1");
        assert_eq!(tabs["tabs"][1]["layout"]["pane"]["focused"], true);
    }
}
