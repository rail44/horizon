use super::PaneId;

#[derive(Clone, Debug)]
pub(crate) enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitAxis {
    Horizontal,
}
