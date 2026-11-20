//! A split's shares changed from the keyboard (`docs/workspace-layout-design.md`):
//! the innermost split around the workspace-mode cursor, along an axis,
//! gives the part the cursor is in a step more or less, or has its parts
//! made equal. The pointer changes the same shares by dragging a boundary.

use super::types::{LayoutChild, LayoutNode, NodeKey, SplitAxis, Workspace};

/// How much of a split one step gives or takes.
pub const RESIZE_STEP: f32 = 0.05;

/// The least a part is left with, of what it shares with its neighbor: as
/// a boundary dragged leaves it.
const LEAST: f32 = 0.1;

/// Where a node `key` names is, as the search goes up the tree.
enum Found {
    /// Not under this node.
    Elsewhere,
    /// Under this node, and no split under it was the one wanted.
    Under,
    /// The split wanted was found, and changed or not.
    Done(bool),
}

impl LayoutNode {
    /// Finds the innermost split around the node `key` names whose axis
    /// `wanted` takes, and has `change` change its children, told the
    /// place of the one the node is in.
    fn around(
        &mut self,
        key: NodeKey,
        wanted: &dyn Fn(SplitAxis) -> bool,
        change: &mut dyn FnMut(&mut Vec<LayoutChild>, usize) -> bool,
    ) -> Found {
        if self.is(key) {
            return Found::Under;
        }
        match self {
            Self::Pane(_) => Found::Elsewhere,
            Self::Tabs(group) => (group.tabs.iter_mut())
                .map(|tab| tab.root.around(key, wanted, change))
                .find(|found| !matches!(found, Found::Elsewhere))
                .unwrap_or(Found::Elsewhere),
            Self::Split { axis, children } => {
                for index in 0..children.len() {
                    match children[index].node.around(key, wanted, change) {
                        Found::Elsewhere => continue,
                        Found::Under if wanted(*axis) && children.len() > 1 => {
                            return Found::Done(change(children, index));
                        }
                        found => return found,
                    }
                }
                Found::Elsewhere
            }
        }
    }
}

/// Gives the child at `index` `step` of the whole (or takes it, where
/// negative) from the one after it, or before it where it is the last.
fn give(children: &mut [LayoutChild], index: usize, step: f32) -> bool {
    let total: f32 = children.iter().map(|child| child.weight).sum();
    if !total.is_finite() || total <= 0.0 {
        return false;
    }
    let other = if index + 1 < children.len() {
        index + 1
    } else {
        index - 1
    };
    let mut shares: Vec<f32> = (children.iter())
        .map(|child| child.weight / total)
        .collect();
    let both = shares[index] + shares[other];
    let least = both * LEAST;
    let now = (shares[index] + step).clamp(least, both - least);
    if (now - shares[index]).abs() < f32::EPSILON {
        return false;
    }
    shares[index] = now;
    shares[other] = both - now;
    for (child, share) in children.iter_mut().zip(shares) {
        child.weight = share;
    }
    true
}

impl Workspace {
    /// Gives the part the workspace-mode cursor is in `step` more of the
    /// innermost split around it along `axis` (less, where negative).
    /// `false`, a no-op, with no cursor, no such split, or the part at its
    /// least or most already.
    pub fn resize_cursor(&mut self, axis: SplitAxis, step: f32) -> bool {
        let Some(cursor) = self.workspace_mode.cursor() else {
            return false;
        };
        let found = self
            .root
            .around(cursor.key(), &|own| own == axis, &mut |children, index| {
                give(children, index, step)
            });
        matches!(found, Found::Done(true))
    }

    /// Makes the parts of the innermost split around the workspace-mode
    /// cursor, along either axis, equal. `false`, a no-op, with no cursor,
    /// no split around it, or its parts equal already.
    pub fn equalize_cursor(&mut self) -> bool {
        let Some(cursor) = self.workspace_mode.cursor() else {
            return false;
        };
        let found = self
            .root
            .around(cursor.key(), &|_| true, &mut |children, _| {
                let even = 1.0 / children.len() as f32;
                let total: f32 = children.iter().map(|child| child.weight).sum();
                let equal = (children.iter())
                    .all(|child| (child.weight / total - even).abs() < f32::EPSILON);
                for child in children.iter_mut() {
                    child.weight = even;
                }
                !equal
            });
        matches!(found, Found::Done(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{Cursor, WorkspaceMode};
    use crate::types::{Pane, PaneId, PluginPane, TabGroup};

    fn pane(workspace: &mut Workspace) -> PaneId {
        let pane = Pane::new(
            PluginPane {
                name: "view".to_string(),
                title: "view".to_string(),
                state: serde_json::Value::Null,
            },
            None,
        );
        let id = pane.id;
        workspace.panes.push(pane);
        id
    }

    fn child(node: LayoutNode) -> LayoutChild {
        LayoutChild { node, weight: 1.0 }
    }

    /// `left` beside a group whose tab is `top` over `bottom`.
    fn workspace() -> (Workspace, PaneId, PaneId, PaneId) {
        let mut workspace = Workspace::mvp();
        let (left, top, bottom) = (
            pane(&mut workspace),
            pane(&mut workspace),
            pane(&mut workspace),
        );
        let in_tab = LayoutNode::Split {
            axis: SplitAxis::Vertical,
            children: vec![
                child(LayoutNode::Pane(top)),
                child(LayoutNode::Pane(bottom)),
            ],
        };
        let group = TabGroup::of(in_tab, top);
        workspace.root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                child(LayoutNode::Pane(left)),
                child(LayoutNode::Tabs(group)),
            ],
        };
        workspace.note_focus(left);
        (workspace, left, top, bottom)
    }

    fn on(workspace: &mut Workspace, pane: PaneId) {
        workspace.workspace_mode = WorkspaceMode::Commands {
            cursor: Some(Cursor::Pane(pane)),
        };
    }

    /// The shares of the split at the root, and of the one in the tab.
    fn shares(workspace: &Workspace) -> (Vec<f32>, Vec<f32>) {
        let of = |children: &[LayoutChild]| -> Vec<f32> {
            let total: f32 = children.iter().map(|child| child.weight).sum();
            (children.iter())
                .map(|child| (child.weight / total * 100.).round() / 100.)
                .collect()
        };
        let LayoutNode::Split { children, .. } = &workspace.root else {
            panic!("a split at the root");
        };
        let LayoutNode::Tabs(group) = &children[1].node else {
            panic!("a group");
        };
        let LayoutNode::Split {
            children: inner, ..
        } = &group.tabs[0].root
        else {
            panic!("a split in the tab");
        };
        (of(children), of(inner))
    }

    #[test]
    fn a_step_goes_to_the_innermost_split_along_the_axis() {
        let (mut workspace, left, top, _) = workspace();
        on(&mut workspace, top);
        // Across: the root's split, the group being the part the pane is in.
        assert!(workspace.resize_cursor(SplitAxis::Horizontal, RESIZE_STEP));
        assert_eq!(shares(&workspace), (vec![0.45, 0.55], vec![0.5, 0.5]));
        // Down: the split in the tab.
        assert!(workspace.resize_cursor(SplitAxis::Vertical, -RESIZE_STEP));
        assert_eq!(shares(&workspace), (vec![0.45, 0.55], vec![0.45, 0.55]));
        // The last part takes from the one before it.
        on(&mut workspace, left);
        assert!(workspace.resize_cursor(SplitAxis::Horizontal, RESIZE_STEP));
        assert_eq!(shares(&workspace).0, [0.5, 0.5]);
        // No split down around `left`.
        assert!(!workspace.resize_cursor(SplitAxis::Vertical, RESIZE_STEP));
    }

    #[test]
    fn a_part_keeps_its_least_and_equal_parts_are_made_again() {
        let (mut workspace, left, ..) = workspace();
        on(&mut workspace, left);
        for _ in 0..20 {
            workspace.resize_cursor(SplitAxis::Horizontal, -RESIZE_STEP);
        }
        assert_eq!(shares(&workspace).0, [0.1, 0.9]);
        assert!(!workspace.resize_cursor(SplitAxis::Horizontal, -RESIZE_STEP));
        assert!(workspace.equalize_cursor());
        assert_eq!(shares(&workspace).0, [0.5, 0.5]);
        assert!(!workspace.equalize_cursor());
    }

    #[test]
    fn nothing_changes_with_no_cursor() {
        let (mut workspace, ..) = workspace();
        assert!(!workspace.resize_cursor(SplitAxis::Horizontal, RESIZE_STEP));
        assert!(!workspace.equalize_cursor());
    }
}
