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
use horizon_ui::reactive_graph::owner::{Owner, on_cleanup};
use horizon_ui::reactive_graph::traits::{Get, Set, Update, With, WithUntracked, WithValue};
use horizon_ui::{El, dispatched, el, prevent_default, stop_propagation};
use horizon_workspace::types::{LayoutNode, NodeKey, TabGroupId, TabId, TabSummary};
use horizon_workspace::{Cursor, PaneId, SplitAxis, Workspace};

use super::{Placement, Shell, activity, chooser, picker, styles};

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

/// The flex-grow of the child at `path` in the workspace's tree: its
/// weight as a share of its split's, times the split's children. A path
/// counts a split's children only: a tab group is passed through into the
/// tab it shows.
///
/// The model's weights are relative, and summing under one once a child
/// is gone from a split whose shares were set; flex-grow summing under
/// one hands out that part of the free space only, and leaves the rest
/// blank. Times the children, they sum to their count.
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
            let total: f32 = children.iter().map(|child| child.weight).sum();
            match children.get(*last) {
                Some(child) if total > 0. => child.weight / total * children.len() as f32,
                _ => 1.,
            }
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
        (shell.kept().strip_elements.borrow_mut()).push((group, strip));
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
        let top = arrange(&shape, &[], shell, &strips, None);
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

/// The edges of a tab's card a part of it is at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Edges {
    pub(super) top: bool,
    pub(super) right: bool,
    pub(super) bottom: bool,
    pub(super) left: bool,
}

impl Edges {
    const ALL: Self = Self {
        top: true,
        right: true,
        bottom: true,
        left: true,
    };

    /// The edges the child at `index` of `count` is at, along a split
    /// across (side by side) or down.
    fn of_child(self, across: bool, index: usize, count: usize) -> Self {
        let (first, last) = (index == 0, index + 1 == count);
        match across {
            true => Self {
                left: self.left && first,
                right: self.right && last,
                ..self
            },
            false => Self {
                top: self.top && first,
                bottom: self.bottom && last,
                ..self
            },
        }
    }
}

/// Builds the boxes of `shape`: `in_tab` the edges of a tab's card the
/// shape is at, where it is a part of a split tab.
fn arrange(
    shape: &Shape,
    path: &[usize],
    shell: Shell,
    strips: &Strips,
    in_tab: Option<Edges>,
) -> El {
    let (axis, children) = match shape {
        Shape::Pane(id) => {
            let (element, place) = {
                let kept = shell.kept();
                let panes = kept.panes.borrow();
                (panes[id].element, panes[id].place)
            };
            place.set(in_tab);
            return element;
        }
        Shape::Group { id, shown } => {
            let alone = matches!(
                shown.as_ref().map(|(_, shape)| &**shape),
                Some(Shape::Pane(_))
            );
            let parted = matches!(
                shown.as_ref().map(|(_, shape)| &**shape),
                Some(Shape::Split { .. })
            );
            let content = match shown {
                // (A path counts a split's children: the group is passed
                // through.)
                // (A tab that is one pane is that pane's card.)
                Some((_, shape)) => {
                    let parts = parted.then_some(Edges::ALL);
                    arrange(shape, path, shell, strips, parts)
                }
                // With no tab, the views a tab can show, to pick one from.
                None => {
                    let search = picker::Search::new();
                    let choices = el("div").class(styles::CHOICES).child(chooser::view(
                        shell,
                        search,
                        Placement::NewTab,
                    ));
                    shell.kept().choices.set(Some(choices));
                    el("div").class(styles::EMPTY).child(choices)
                }
            };
            return el("div")
                .class(styles::GROUP)
                .child(strips.of(shell, *id))
                .child(
                    el("div")
                        .class(styles::SHOWN_TAB)
                        .class((
                            alone.then_some(styles::ALONE),
                            parted.then_some(styles::PARTED),
                        ))
                        .child(content),
                );
        }
        Shape::Split { axis, children } => (*axis, children),
    };
    let across = axis == SplitAxis::Horizontal;
    // (A split in a tab is told apart from one between cards.)
    let split = el("div")
        .class((
            styles::SPLIT,
            if across { styles::ACROSS } else { styles::DOWN },
        ))
        .class(in_tab.is_some().then_some(styles::IN_TAB));
    let count = children.len();
    let cells: Vec<El> = (children.iter().enumerate())
        .map(|(index, child)| {
            let at = [path, &[index]].concat();
            let weighed = at.clone();
            let edges = in_tab.map(|edges| edges.of_child(across, index, count));
            el("div")
                .class(styles::CELL)
                .style_dyn("flex-grow", move || {
                    shell
                        .model
                        .with(|model| weight(model, &weighed))
                        .to_string()
                })
                .child(arrange(child, &at, shell, strips, edges))
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
                    .child(el("div").class(styles::GRIP))
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
            (
                (on == Some(Cursor::Strip(group)) && !shell.lent_shown()).then_some(styles::CURSOR),
                // (With no label to draw the line by, the strip is marked.)
                (shell.drop_between(group).is_some() && tabs.with(Vec::is_empty))
                    .then_some(styles::DROP),
            )
        })
        .each(
            move || (0..tabs.with(Vec::len)).collect::<Vec<_>>(),
            |index| *index,
            move |index| {
                let index = *index;
                let label = el("div");
                // (Where a view moving can go, while it is on the page.)
                shell
                    .kept()
                    .tab_labels
                    .borrow_mut()
                    .push((label, group, index));
                on_cleanup(move || {
                    if let Some(kept) = shell.kept.try_with_value(Rc::clone) {
                        kept.tab_labels.borrow_mut().retain(|(of, ..)| *of != label);
                    }
                });
                // Where a view moving would go between the tabs: a line
                // before this label, or after it when it is the last.
                let line = move || {
                    let last = tabs.with(Vec::len) == index + 1;
                    match shell.drop_between(group) {
                        Some(at) if at == index => Some(styles::TO_LEFT),
                        Some(at) if at == index + 1 && last => Some(styles::TO_RIGHT),
                        _ => None,
                    }
                };
                label
                    .class(styles::TAB_LABEL)
                    .when(
                        move || line().is_some(),
                        move || el("div").class(styles::LANDING).class_dyn(line),
                    )
                    .class_dyn(move || {
                        tab(index)
                            .is_some_and(|tab| tab.active)
                            .then_some(styles::ACTIVE)
                    })
                    // (A view's cursor, on a pane of this tab, not shown.)
                    .class_dyn(move || {
                        shell
                            .lent_cursor_on_tab(group, index)
                            .then_some(styles::CURSOR)
                    })
                    // (The group with the focus, or the cursor in the mode.)
                    .class_dyn(move || {
                        (shell.model.with(Workspace::cursor_group) == Some(group))
                            .then_some(styles::FOCUSED)
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
                    // (Taken by its label, a tab moves its active pane.)
                    .on("pointerdown", move |event| {
                        let DomEventData::PointerDown(pointer) = &event.data else {
                            return;
                        };
                        let pane = (shell.model)
                            .with_untracked(|model| model.tab_active_pane(group, index));
                        if let Some(pane) = pane {
                            shell.begin_moving(pane, pointer.page_x(), pointer.page_y());
                        }
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
