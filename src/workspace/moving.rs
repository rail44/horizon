//! A view moved with the pointer (`docs/workspace-layout-design.md`,
//! board #94): taken by its tab's label, or, where the label stands for
//! more than the pane, by the grip with its title in the pane's top right
//! corner, which shows while the pointer is near the pane's top edge;
//! dropped on another
//! view, the two swap places; dropped on a tab group's strip, it is a new
//! tab of that group. What is under the pointer is marked while it moves,
//! and the view's title goes along beside the pointer.
//!
//! A press that does not go far is no move: a click on a tab's label is
//! still the click.

use horizon_components::Icon;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_ui::{El, dispatched, el, prevent_default, stop_propagation};
use horizon_workspace::PaneId;
use horizon_workspace::types::{NodeKey, TabGroupId};

use super::{Shell, styles};

/// How far the pointer goes from where it was pressed before a press is
/// a move, in pixels.
const FAR_ENOUGH: f32 = 4.0;

/// How near a pane's top edge the pointer is for its grip to show, in
/// pixels.
const NEAR_THE_TOP: f64 = 16.0;

/// Where a view moved would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// In this pane's place, the pane in the moved one's.
    Pane(PaneId),
    /// A new tab of this group.
    Strip(TabGroupId),
}

/// A view being moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Moving {
    pane: PaneId,
    /// Where the pointer was pressed.
    from: (f32, f32),
    /// Where the pointer is.
    at: (f32, f32),
    /// Whether the pointer has gone far enough from `from` to be a move.
    going: bool,
    over: Option<Target>,
}

impl Shell {
    /// The pointer pressed on `pane`'s grip, or its tab's label, at `x`,
    /// `y`.
    pub(super) fn begin_moving(&self, pane: PaneId, x: f32, y: f32) {
        self.moving.set(Some(Moving {
            pane,
            from: (x, y),
            at: (x, y),
            going: false,
            over: None,
        }));
    }

    /// Whether `target` is where the view moving would go.
    pub(super) fn is_drop(&self, target: Target) -> bool {
        self.moving
            .with(|moving| moving.is_some_and(|moving| moving.going && moving.over == Some(target)))
    }

    /// Whether `pane` is the one moving.
    pub(super) fn is_moving(&self, pane: PaneId) -> bool {
        self.moving
            .with(|moving| moving.is_some_and(|moving| moving.going && moving.pane == pane))
    }

    /// Whether `pane`'s grip shows: the pointer near its top edge, its
    /// tab's label not standing for it alone, and nothing moving.
    pub(super) fn grip_shows(&self, pane: PaneId) -> bool {
        self.near_top.get() == Some(pane)
            && !self.moving_any()
            && !self.model.with(|model| model.pane_is_its_tab(pane))
    }

    /// Whether a view is moving: the page's text is not selected meanwhile.
    pub(super) fn moving_any(&self) -> bool {
        self.moving
            .with(|moving| moving.is_some_and(|moving| moving.going))
    }

    /// What is at a point of the page, as where a view would go: the
    /// innermost pane or strip around the element drawn there.
    fn target_in(&self, path: &[El]) -> Option<Target> {
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let strips = kept.strip_elements.borrow();
        path.iter().find_map(|element| {
            let pane = (panes.iter()).find(|(_, view)| view.element == *element);
            if let Some((id, _)) = pane {
                return Some(Target::Pane(*id));
            }
            (strips.iter())
                .find(|(_, strip)| strip == element)
                .map(|(group, _)| Target::Strip(*group))
        })
    }
}

/// The grip in the top right corner of `pane`'s pane, with the view's
/// title: shown while the pointer is near its top edge, it takes the view
/// to move it. (In the corner, it is clear of a terminal's text, which
/// begins at the left.)
pub(super) fn grip(shell: Shell, pane: PaneId) -> El {
    let handle = el("div")
        .class(styles::HANDLE)
        .attr("title", "Drag to move this view")
        .child(Icon::DotsSixVertical)
        .child(el("span").text_dyn(move || {
            (shell.model.with(|model| model.pane_label(pane))).unwrap_or_default()
        }));
    el("div")
        .class(styles::GRIP)
        .class_dyn(move || shell.grip_shows(pane).then_some(styles::SHOWN))
        .child(handle.on("pointerdown", move |event| {
            if let DomEventData::PointerDown(pointer) = &event.data {
                shell.begin_moving(pane, pointer.page_x(), pointer.page_y());
            }
            // (Not a press in the pane: its view keeps the focus as it
            // was, and nothing is selected.)
            prevent_default();
            stop_propagation();
        }))
}

/// The pointer moved over the workspace: a view being moved goes on, and
/// what is under the pointer is where it would go.
pub(super) fn moved(shell: Shell, event: &DomEvent) {
    let DomEventData::PointerMove(pointer) = &event.data else {
        return;
    };
    let (x, y) = (pointer.page_x(), pointer.page_y());
    // Which pane's top edge the pointer is near, if one's.
    dispatched(move |page| {
        let path = page.under(x, y);
        let near = match shell.target_in(&path) {
            Some(Target::Pane(pane)) => {
                let element = shell
                    .kept()
                    .panes
                    .borrow()
                    .get(&pane)
                    .map(|view| view.element);
                let top = element
                    .and_then(|element| page.bounds(element))
                    .map(|at| at.y);
                top.filter(|top| f64::from(y) - top <= NEAR_THE_TOP)
                    .map(|_| pane)
            }
            _ => None,
        };
        if shell.near_top.get_untracked() != near {
            shell.near_top.set(near);
        }
    });
    let Some(mut moving) = shell.moving.get_untracked() else {
        return;
    };
    moving.at = (x, y);
    let (dx, dy) = (x - moving.from.0, y - moving.from.1);
    moving.going |= dx * dx + dy * dy >= FAR_ENOUGH * FAR_ENOUGH;
    shell.moving.set(Some(moving));
    if !moving.going {
        return;
    }
    dispatched(move |page| {
        let over = shell.target_in(&page.under(x, y));
        // (Not onto itself.)
        let over = over.filter(|over| *over != Target::Pane(moving.pane));
        shell.moving.update(|now| {
            if let Some(now) = now {
                now.over = over;
            }
        });
    });
}

/// The pointer's button came up: a view being moved goes where it was
/// over, if anywhere.
pub(super) fn released(shell: Shell) {
    let Some(moving) = shell.moving.get_untracked() else {
        return;
    };
    shell.moving.set(None);
    if !moving.going {
        return;
    }
    match moving.over {
        Some(Target::Pane(pane)) => shell.change(|model| {
            _ = model.swap_nodes(NodeKey::Pane(moving.pane), NodeKey::Pane(pane));
        }),
        Some(Target::Strip(group)) => shell.change(|model| {
            _ = model.move_pane_to_new_tab(moving.pane, group);
        }),
        None => {}
    }
}

/// The view's title beside the pointer, while it is moved.
pub(super) fn carried(shell: Shell) -> El {
    el("div")
        .class(styles::CARRIED)
        .style_dyn("left", move || {
            let x = shell
                .moving
                .with(|moving| moving.map_or(0., |moving| moving.at.0));
            format!("{}px", x + 14.)
        })
        .style_dyn("top", move || {
            let y = shell
                .moving
                .with(|moving| moving.map_or(0., |moving| moving.at.1));
            format!("{}px", y + 14.)
        })
        .text_dyn(move || {
            let pane = shell.moving.with(|moving| moving.map(|moving| moving.pane));
            (pane.and_then(|pane| shell.model.with_untracked(|model| model.pane_label(pane))))
                .unwrap_or_default()
        })
        .class_dyn(move || {
            (!shell.moving.get().is_some_and(|moving| moving.going)).then_some(styles::HIDDEN)
        })
}
