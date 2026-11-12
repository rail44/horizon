//! The active tab's panes in their splits, built again around the same
//! pane elements whenever the splits change, and the boundaries between
//! them to drag.

use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, Update, With, WithUntracked};
use horizon_ui::{El, dispatched, el, prevent_default};
use horizon_workspace::types::{LayoutNode, TabId};
use horizon_workspace::{PaneId, SplitAxis, Workspace};

use super::Shell;

/// A tab's splits without their weights: what the boxes are built from. A
/// boundary dragged changes weights only, and builds nothing.
#[derive(Clone, PartialEq)]
enum Shape {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: Vec<Shape>,
    },
}

impl Shape {
    fn of(node: &LayoutNode) -> Self {
        match node {
            LayoutNode::Pane(id) => Shape::Pane(*id),
            LayoutNode::Split { axis, children } => Shape::Split {
                axis: *axis,
                children: children
                    .iter()
                    .map(|child| Shape::of(&child.node))
                    .collect(),
            },
        }
    }

    fn panes(&self) -> Vec<PaneId> {
        match self {
            Shape::Pane(id) => vec![*id],
            Shape::Split { children, .. } => children.iter().flat_map(Shape::panes).collect(),
        }
    }

    fn first_pane(&self) -> PaneId {
        match self {
            Shape::Pane(id) => *id,
            Shape::Split { children, .. } => children[0].first_pane(),
        }
    }
}

/// The weight of the child at `path` in the active tab's splits.
fn weight(model: &Workspace, path: &[usize]) -> f32 {
    let Some((last, parents)) = path.split_last() else {
        return 1.;
    };
    let mut node = model.active_tab().map(|tab| &tab.root);
    for index in parents {
        node = match node {
            Some(LayoutNode::Split { children, .. }) => {
                children.get(*index).map(|child| &child.node)
            }
            _ => None,
        };
    }
    match node {
        Some(LayoutNode::Split { children, .. }) => {
            children.get(*last).map_or(1., |child| child.weight)
        }
        _ => 1.,
    }
}

/// A boundary being dragged: the split's cells, the one before the
/// boundary, and how the model names the split.
#[derive(Clone)]
pub(super) struct Resize {
    cells: Vec<El>,
    before: usize,
    across: bool,
    tab: TabId,
    anchor: PaneId,
    anchors: Vec<PaneId>,
}

pub(super) fn view(shell: Shell) -> El {
    let shown = Memo::new(move |_| {
        (shell.model).with(|model| model.active_tab().map(|tab| (tab.id, Shape::of(&tab.root))))
    });
    let root = el("div").class("tab");
    // The outermost box of the last arrangement, when it was a split, and
    // the panes that were in it.
    Effect::new(move |previous: Option<(Option<El>, Vec<El>)>| {
        let shown = shown.get();
        shell.reconcile();
        let (split, panes) = previous.unwrap_or_default();
        for pane in panes {
            pane.detach();
        }
        // With the boxes inside it.
        if let Some(split) = split {
            split.remove();
        }
        let Some((tab, shape)) = shown else {
            return (None, Vec::new());
        };
        let top = arrange(&shape, &[], tab, shell);
        root.child(top);
        let kept = shell.kept();
        let panes: Vec<El> = (shape.panes().iter())
            .filter_map(|id| kept.panes.borrow().get(id).map(|view| view.element))
            .collect();
        ((!panes.contains(&top)).then_some(top), panes)
    });
    root.when(
        move || shown.with(Option::is_none),
        || {
            el("div")
                .class("empty")
                .text("No tabs. Press : for commands.")
        },
    )
}

fn arrange(shape: &Shape, path: &[usize], tab: TabId, shell: Shell) -> El {
    let (axis, children) = match shape {
        Shape::Pane(id) => return shell.kept().panes.borrow()[id].element,
        Shape::Split { axis, children } => (*axis, children),
    };
    let across = axis == SplitAxis::Horizontal;
    let split = el("div").class(if across { "split across" } else { "split down" });
    let cells: Vec<El> = (children.iter().enumerate())
        .map(|(index, child)| {
            let at = [path, &[index]].concat();
            let weighed = at.clone();
            el("div")
                .class("cell")
                .style_dyn("flex-grow", move || {
                    shell
                        .model
                        .with(|model| weight(model, &weighed))
                        .to_string()
                })
                .child(arrange(child, &at, tab, shell))
        })
        .collect();
    let resize = Resize {
        cells: cells.clone(),
        before: 0,
        across,
        tab,
        anchor: shape.first_pane(),
        anchors: children.iter().map(Shape::first_pane).collect(),
    };
    for (index, cell) in cells.into_iter().enumerate() {
        if index > 0 {
            let resize = Resize {
                before: index - 1,
                ..resize.clone()
            };
            split.child(el("div").class("divider").on("pointerdown", move |_| {
                let resize = resize.clone();
                shell.resizing.update(|resizing| *resizing = Some(resize));
                // Not the start of a selection in the text around it.
                prevent_default();
            }));
        }
        split.child(cell);
    }
    split
}

/// The pointer moved: the boundary being dragged, if one is, follows it.
pub(super) fn dragged(shell: Shell, event: &DomEvent) {
    let DomEventData::PointerMove(pointer) = &event.data else {
        return;
    };
    let Some(resize) = shell.resizing.with_untracked(Clone::clone) else {
        return;
    };
    let pointer = match resize.across {
        true => pointer.page_x() as f64,
        false => pointer.page_y() as f64,
    };
    dispatched(move |page| {
        // Each cell's start and extent along the split.
        let extents: Option<Vec<(f64, f64)>> = (resize.cells.iter())
            .map(|cell| {
                let bounds = page.bounds(*cell)?;
                Some(match resize.across {
                    true => (bounds.x, bounds.width),
                    false => (bounds.y, bounds.height),
                })
            })
            .collect();
        let Some(extents) = extents else {
            return;
        };
        // The two cells at the boundary share what they had between them.
        let (start, first) = extents[resize.before];
        let both = first + extents[resize.before + 1].1;
        let least = both * 0.1;
        let first = (pointer - start).clamp(least, both - least);
        let mut sizes: Vec<f32> = extents.iter().map(|(_, extent)| *extent as f32).collect();
        sizes[resize.before] = first as f32;
        sizes[resize.before + 1] = (both - first) as f32;
        shell.model.update(|model| {
            model.set_split_weights(resize.tab, resize.anchor, &resize.anchors, &sizes);
        });
    });
}
