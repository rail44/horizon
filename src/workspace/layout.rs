use super::types::{LayoutNode, PaneId};

impl LayoutNode {
    pub(super) fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            Self::Pane(pane_id) => vec![*pane_id],
            Self::Split { first, second, .. } => {
                let mut panes = first.pane_ids();
                panes.extend(second.pane_ids());
                panes
            }
        }
    }

    pub(super) fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Pane(pane_id) => Some(*pane_id),
            Self::Split { first, second, .. } => first.first_pane().or_else(|| second.first_pane()),
        }
    }

    pub(super) fn without_pane(&self, pane_id: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) if *id == pane_id => None,
            Self::Pane(id) => Some(Self::Pane(*id)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without_pane(pane_id), second.without_pane(pane_id)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}
