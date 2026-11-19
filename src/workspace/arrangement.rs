//! The workspace's tree on the page: its panes in their splits, and each
//! tab group as its strip above the tab it shows -- built again around the
//! same pane elements whenever the tree's shape changes -- and the
//! boundaries between them to drag.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::Owner;
use horizon_ui::reactive_graph::traits::{Get, Update, With, WithUntracked};
use horizon_ui::{El, dispatched, el, prevent_default, stop_propagation};
use horizon_workspace::types::{LayoutNode, NodeKey, TabGroupId, TabId, TabSummary};
use horizon_workspace::{Cursor, PaneId, SplitAxis, Workspace};

use super::{Shell, activity, styles};

/// The tree without its weights: what the boxes are built from. A
/// boundary dragged changes weights only, and builds nothing.
#[derive(Clone, PartialEq)]
enum Shape {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: Vec<Shape>,
    },
    /// A tab group, and the tab it shows with that tab's shape; none for
    /// the empty workspace's root.
    Group {
        id: TabGroupId,
        shown: Option<(TabId, Box<Shape>)>,
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
            LayoutNode::Tabs(group) => Shape::Group {
                id: group.id,
                shown: (group.shown_tab()).map(|tab| (tab.id, Box::new(Shape::of(&tab.root)))),
            },
        }
    }

    /// The tab groups on the page.
    fn groups(&self) -> Vec<TabGroupId> {
        match self {
            Shape::Pane(_) => Vec::new(),
            Shape::Split { children, .. } => children.iter().flat_map(Shape::groups).collect(),
            Shape::Group { id, shown } => std::iter::once(*id)
                .chain(shown.iter().flat_map(|(_, shape)| shape.groups()))
                .collect(),
        }
    }

    /// The panes on the page, in order.
    fn panes(&self) -> Vec<PaneId> {
        match self {
            Shape::Pane(id) => vec![*id],
            Shape::Split { children, .. } => children.iter().flat_map(Shape::panes).collect(),
            Shape::Group { shown, .. } => (shown.iter())
                .flat_map(|(_, shape)| shape.panes())
                .collect(),
        }
    }

    /// The key the model knows this node by, as `set_split_weights` names
    /// a split's children.
    fn first_key(&self) -> NodeKey {
        match self {
            Shape::Pane(id) => NodeKey::Pane(*id),
            Shape::Group { id, .. } => NodeKey::Group(*id),
            Shape::Split { children, .. } => (children.first())
                .map(Shape::first_key)
                .unwrap_or(NodeKey::Pane(PaneId::default())),
        }
    }
}

/// The weight of the child at `path` in the workspace's tree. A path
/// counts a split's children only: a tab group is passed through into the
/// tab it shows.
fn weight(model: &Workspace, path: &[usize]) -> f32 {
    /// The node itself, or the tree of the tab it shows for a group.
    fn through(mut node: &LayoutNode) -> Option<&LayoutNode> {
        while let LayoutNode::Tabs(group) = node {
            node = &group.shown_tab()?.root;
        }
        Some(node)
    }
    let Some((last, parents)) = path.split_last() else {
        return 1.;
    };
    let mut node = Some(model.layout());
    for index in parents {
        node = match node.and_then(through) {
            Some(LayoutNode::Split { children, .. }) => {
                children.get(*index).map(|child| &child.node)
            }
            _ => None,
        };
    }
    match node.and_then(through) {
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
    anchor: NodeKey,
    anchors: Vec<NodeKey>,
}

/// Each tab group's strip, kept across arrangements as the panes are --
/// the boxes around them are built again, the strips are moved -- with
/// what it was made under, to let go of once its group is gone.
#[derive(Clone)]
struct Strips {
    owner: Owner,
    made: Rc<RefCell<HashMap<TabGroupId, (El, Owner)>>>,
}

impl Strips {
    /// `group`'s strip, made the first time it is asked for.
    fn of(&self, shell: Shell, group: TabGroupId) -> El {
        if let Some((strip, _)) = self.made.borrow().get(&group) {
            return *strip;
        }
        let owner = self.owner.child();
        let strip = owner.with(|| strip(shell, group));
        self.made.borrow_mut().insert(group, (strip, owner));
        strip
    }

    /// Takes every strip off the page, to be put in the boxes built next.
    fn detach_all(&self) {
        for (strip, _) in self.made.borrow().values() {
            strip.detach();
        }
    }

    /// Lets go of the strips of groups that are not in `shape`.
    fn keep_only(&self, shape: &Shape) {
        let groups = shape.groups();
        self.made.borrow_mut().retain(|group, (strip, owner)| {
            let kept = groups.contains(group);
            if !kept {
                owner.cleanup();
                strip.remove();
            }
            kept
        });
    }
}

pub(super) fn view(shell: Shell) -> El {
    let shape = Memo::new(move |_| (shell.model).with(|model| Shape::of(model.layout())));
    let root = el("div").class(styles::TAB);
    let strips = Strips {
        owner: Owner::current().expect("an owner"),
        made: Rc::default(),
    };
    // The outermost box of the last arrangement, when it was not a pane,
    // and the panes that were in it.
    Effect::new(move |previous: Option<(Option<El>, Vec<El>)>| {
        let shape = shape.get();
        shell.rearranged.get();
        shell.reconcile();
        let (outer, panes) = previous.unwrap_or_default();
        // A terminal on the page has a terminal of its own to draw from,
        // and one on a tab that is not shown does not.
        let on_the_page = shape.panes();
        for (id, view) in shell.kept().panes.borrow().iter() {
            if let Some((_, terminal)) = &view.terminal {
                terminal.shown(on_the_page.contains(id));
            }
        }
        for pane in panes {
            pane.detach();
        }
        strips.detach_all();
        // With the boxes inside it.
        if let Some(outer) = outer {
            outer.remove();
        }
        let top = arrange(&shape, &[], shell, &strips);
        strips.keep_only(&shape);
        root.child(top);
        let kept = shell.kept();
        if kept.refocus.take() {
            shell.focus_active();
        }
        let panes: Vec<El> = (shape.panes().iter())
            .filter_map(|id| kept.panes.borrow().get(id).map(|view| view.element))
            .collect();
        ((!panes.contains(&top)).then_some(top), panes)
    });
    root
}

fn arrange(shape: &Shape, path: &[usize], shell: Shell, strips: &Strips) -> El {
    let (axis, children) = match shape {
        Shape::Pane(id) => return shell.kept().panes.borrow()[id].element,
        Shape::Group { id, shown } => {
            let content = match shown {
                // (A path counts a split's children: the group is passed
                // through.)
                Some((_, shape)) => arrange(shape, path, shell, strips),
                None => el("div")
                    .class(styles::EMPTY)
                    .text("No tabs. Press : for commands."),
            };
            return el("div")
                .class(styles::GROUP)
                .child(strips.of(shell, *id))
                .child(el("div").class(styles::SHOWN_TAB).child(content));
        }
        Shape::Split { axis, children } => (*axis, children),
    };
    let across = axis == SplitAxis::Horizontal;
    let split = el("div").class((
        styles::SPLIT,
        if across { styles::ACROSS } else { styles::DOWN },
    ));
    let cells: Vec<El> = (children.iter().enumerate())
        .map(|(index, child)| {
            let at = [path, &[index]].concat();
            let weighed = at.clone();
            el("div")
                .class(styles::CELL)
                .style_dyn("flex-grow", move || {
                    shell
                        .model
                        .with(|model| weight(model, &weighed))
                        .to_string()
                })
                .child(arrange(child, &at, shell, strips))
        })
        .collect();
    let resize = Resize {
        cells: cells.clone(),
        before: 0,
        across,
        anchor: shape.first_key(),
        anchors: children.iter().map(Shape::first_key).collect(),
    };
    for (index, cell) in cells.into_iter().enumerate() {
        if index > 0 {
            let resize = Resize {
                before: index - 1,
                ..resize.clone()
            };
            split.child(
                el("div")
                    .class(styles::DIVIDER)
                    .on("pointerdown", move |_| {
                        let resize = resize.clone();
                        shell.resizing.update(|resizing| *resizing = Some(resize));
                        // Not the start of a selection in the text around it.
                        prevent_default();
                    }),
            );
        }
        split.child(cell);
    }
    split
}

/// A tab group's strip: its tabs, the one it shows marked, each closed
/// with its button and shown with a click. Marked too while the
/// workspace-mode cursor is on it.
fn strip(shell: Shell, group: TabGroupId) -> El {
    let tabs = Memo::new(move |_| shell.model.with(|model| model.group_tab_summaries(group)));
    let tab = move |index: usize| tabs.with(|tabs: &Vec<TabSummary>| tabs.get(index).cloned());
    el("div")
        .class(styles::TABS)
        .class_dyn(move || {
            let on = shell.model.with(|model| model.workspace_cursor());
            (on == Some(Cursor::Strip(group))).then_some(styles::CURSOR)
        })
        .each(
            move || (0..tabs.with(Vec::len)).collect::<Vec<_>>(),
            |index| *index,
            move |index| {
                let index = *index;
                el("div")
                    .class(styles::TAB_LABEL)
                    .class_dyn(move || {
                        tab(index)
                            .is_some_and(|tab| tab.active)
                            .then_some(styles::ACTIVE)
                    })
                    .child(el("div").class(styles::TITLE).text_dyn(move || {
                        tab(index).map_or_default(|tab| {
                            activity::marked(tab.activity.as_ref(), tab.title)
                        })
                    }))
                    // Closing a tab detaches the sessions in it.
                    .child(
                        el("div")
                            .class(styles::CLOSE)
                            .text("×")
                            .on("click", move |_| {
                                shell.change(|model| {
                                    model.exit_workspace_mode();
                                    model.close_tab_in(group, index);
                                });
                                stop_propagation();
                            }),
                    )
                    .on("click", move |_| {
                        shell.change(|model| {
                            model.exit_workspace_mode();
                            model.activate_tab_in(group, index);
                        });
                    })
            },
        )
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
            model.set_split_weights(resize.anchor, &resize.anchors, &sizes);
        });
    });
}
