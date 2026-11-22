//! A view moved with the pointer (`docs/workspace-layout-design.md`,
//! boards #94, #95): taken by its tab's label, or by the pane's tag
//! (`super::tag`), by its grip and name. Dropped on the middle of another view, the two swap
//! places; on an edge of it, it goes beside it on that side; on a tab's
//! label, into that tab, beside its active pane; on the rest of a tab
//! group's strip, it is a new tab of that group. A tab whose label the
//! pointer comes over is shown, so that its views can be gone to. What is
//! under the pointer is marked while it moves, and the view's title goes
//! along beside the pointer.
//!
//! A press that does not go far is no move: a click on a tab's label is
//! still the click.

use horizon_components::Icon;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_ui::{Bounds, El, Page, dispatched, el, prevent_default, stop_propagation};
use horizon_workspace::types::{NodeKey, TabGroupId};
use horizon_workspace::{PaneId, Side};

use super::{Shell, styles};

/// How far the pointer goes from where it was pressed before a press is
/// a move, in pixels.
const FAR_ENOUGH: f32 = 4.0;

/// How far in from a side of a view its edge on that side goes, of its
/// width or height.
const EDGE: f64 = 0.25;

/// Which part of a view the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Zone {
    /// Its middle: the two swap places.
    Middle,
    /// An edge: beside it on that side.
    Edge(Side),
}

/// Where a view moved would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// This pane, by the part of it the pointer is over.
    Pane(PaneId, Zone),
    /// The tab at this place in this group's strip, beside its active pane.
    Tab(TabGroupId, usize),
    /// A new tab of this group.
    Strip(TabGroupId),
}

/// The part of a view's box a point is in.
fn zone_at(bounds: Bounds, x: f32, y: f32) -> Zone {
    let across = (f64::from(x) - bounds.x) / bounds.width;
    let down = (f64::from(y) - bounds.y) / bounds.height;
    let sides = [
        (across, Side::Left),
        (1. - across, Side::Right),
        (down, Side::Up),
        (1. - down, Side::Down),
    ];
    let (nearest, side) = (sides.into_iter())
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("four sides");
    match nearest < EDGE {
        true => Zone::Edge(side),
        false => Zone::Middle,
    }
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
    /// The pointer pressed on `pane`'s tag, or its tab's label, at `x`,
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

    /// The part of `pane` a view moving would go to, if it would go to it.
    pub(super) fn drop_zone(&self, pane: PaneId) -> Option<Zone> {
        self.moving.with(|moving| match moving {
            Some(Moving {
                going: true,
                over: Some(Target::Pane(over, zone)),
                ..
            }) if *over == pane => Some(*zone),
            _ => None,
        })
    }

    /// Whether `pane` is the one moving.
    pub(super) fn is_moving(&self, pane: PaneId) -> bool {
        self.moving
            .with(|moving| moving.is_some_and(|moving| moving.going && moving.pane == pane))
    }

    /// Whether a view is moving: the page's text is not selected meanwhile.
    pub(super) fn moving_any(&self) -> bool {
        self.moving
            .with(|moving| moving.is_some_and(|moving| moving.going))
    }

    /// What is at the point `x`, `y` of the page, as where a view would
    /// go: the innermost pane, tab's label or strip around the element
    /// drawn there. A tab's label is where to go into that tab, but for
    /// the tab `moving` is in, which is its strip's: a new tab.
    fn target_at(&self, page: &Page, x: f32, y: f32, moving: PaneId) -> Option<Target> {
        let own = self.model.with_untracked(|model| model.tab_of(moving));
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let strips = kept.strip_elements.borrow();
        let labels = kept.tab_labels.borrow();
        page.under(x, y).into_iter().find_map(|element| {
            let label = (labels.iter()).find(|(label, ..)| *label == element);
            if let Some((_, group, index)) = label {
                let tab = self
                    .model
                    .with_untracked(|model| model.tab_in(*group, *index));
                return Some(match tab.is_some() && tab == own {
                    true => Target::Strip(*group),
                    false => Target::Tab(*group, *index),
                });
            }
            let pane = (panes.iter()).find(|(_, view)| view.element == element);
            if let Some((id, _)) = pane {
                let zone = (page.bounds(element)).map_or(Zone::Middle, |b| zone_at(b, x, y));
                return Some(Target::Pane(*id, zone));
            }
            (strips.iter())
                .find(|(_, strip)| *strip == element)
                .map(|(group, _)| Target::Strip(*group))
        })
    }
}

/// The pointer moved over the workspace: a view being moved goes on, and
/// what is under the pointer is where it would go.
pub(super) fn moved(shell: Shell, event: &DomEvent) {
    let DomEventData::PointerMove(pointer) = &event.data else {
        return;
    };
    let (x, y) = (pointer.page_x(), pointer.page_y());
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
        let over = shell.target_at(page, x, y, moving.pane);
        // (Not onto itself.)
        let over =
            over.filter(|over| !matches!(over, Target::Pane(pane, _) if *pane == moving.pane));
        let before = shell
            .moving
            .with_untracked(|now| now.and_then(|now| now.over));
        shell.moving.update(|now| {
            if let Some(now) = now {
                now.over = over;
            }
        });
        // A tab's label come over shows its tab, to go to its views.
        if let Some(Target::Tab(group, index)) = over
            && before != over
        {
            shell.model_changed(|model| _ = model.activate_tab_in(group, index));
        }
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
        Some(Target::Pane(pane, Zone::Middle)) => shell.change(|model| {
            _ = model.swap_nodes(NodeKey::Pane(moving.pane), NodeKey::Pane(pane));
        }),
        Some(Target::Pane(pane, Zone::Edge(side))) => shell.change(|model| {
            _ = model.move_pane_beside(moving.pane, pane, side);
        }),
        Some(Target::Tab(group, index)) => shell.change(|model| {
            if let Some(tab) = model.tab_in(group, index) {
                _ = model.move_pane_to_tab(moving.pane, tab);
            }
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
