//! A view moved with the pointer (`docs/workspace-layout-design.md`,
//! boards #94, #95): taken by its tab's label, or, where the label does
//! not name the pane alone, by the pane's tag -- its name and a grip, in
//! its top right corner, shown while the pointer is over the pane or in
//! workspace mode. Dropped on another view, it goes beside it on the side
//! of it the pointer is nearest; on a tab group's strip, it is a new tab
//! of that group, before a tab's label the pointer is on the first half
//! of and after one it is on the second half of (a view alone in its tab
//! moves that tab there). A tab whose label the pointer comes over is
//! shown, so that its views can be gone to. Where the view would go is
//! marked by a line on that side of the view, or between the labels,
//! while it moves, and the view's title goes along beside the pointer.
//!
//! A press that does not go far is no move: a click on a tab's label is
//! still the click.

use horizon_components::Icon;
use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_ui::{Bounds, El, Page, dispatched, el, prevent_default, stop_propagation};
use horizon_workspace::types::TabGroupId;
use horizon_workspace::{PaneId, Side};

use super::{Shell, styles};

/// How far the pointer goes from where it was pressed before a press is
/// a move, in pixels.
const FAR_ENOUGH: f32 = 4.0;

/// Where a view moved would go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// Beside this pane, on this side of it.
    Pane(PaneId, Side),
    /// A new tab of this group, at this place between its tabs (0 before
    /// the first, their count after the last).
    Strip(TabGroupId, usize),
}

/// The side of a view's box a point is nearest, its box's width and
/// height counted as one.
fn side_at(bounds: Bounds, x: f32, y: f32) -> Side {
    let across = (f64::from(x) - bounds.x) / bounds.width;
    let down = (f64::from(y) - bounds.y) / bounds.height;
    let sides = [
        (across, Side::Left),
        (1. - across, Side::Right),
        (down, Side::Up),
        (1. - down, Side::Down),
    ];
    (sides.into_iter())
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, side)| side)
        .expect("four sides")
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
    /// The tab's label the pointer is on, by its group and place.
    label: Option<(TabGroupId, usize)>,
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
            label: None,
        }));
    }

    /// Where the view moving would go, once it is going.
    fn going_to(&self) -> Option<Target> {
        self.moving.with(|moving| {
            moving
                .filter(|moving| moving.going)
                .and_then(|moving| moving.over)
        })
    }

    /// The place between `group`'s tabs a view moving would go to, if it
    /// would go to one.
    pub(super) fn drop_between(&self, group: TabGroupId) -> Option<usize> {
        match self.going_to() {
            Some(Target::Strip(to, at)) if to == group => Some(at),
            _ => None,
        }
    }

    /// The side of `pane` a view moving would go to, if it would go
    /// beside it.
    pub(super) fn drop_side(&self, pane: PaneId) -> Option<Side> {
        match self.going_to() {
            Some(Target::Pane(to, side)) if to == pane => Some(side),
            _ => None,
        }
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
    /// go -- the innermost pane, tab's label or strip around the element
    /// drawn there -- and the tab's label there, if one is. Where `moving`
    /// would stay as it is, alone in a tab and dropped beside that tab's
    /// label, it would go nowhere.
    fn target_at(
        &self,
        page: &Page,
        x: f32,
        y: f32,
        moving: PaneId,
    ) -> (Option<Target>, Option<(TabGroupId, usize)>) {
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let strips = kept.strip_elements.borrow();
        let labels = kept.tab_labels.borrow();
        let mut on_label = None;
        let target = page.under(x, y).into_iter().find_map(|element| {
            let label = (labels.iter()).find(|(label, ..)| *label == element);
            if let Some((_, group, index)) = label {
                on_label = Some((*group, *index));
                // (Before it on its first half, after it on its second.)
                let after =
                    (page.bounds(element)).is_some_and(|b| f64::from(x) >= b.x + b.width / 2.);
                return Some(Target::Strip(*group, index + usize::from(after)));
            }
            let pane = (panes.iter()).find(|(_, view)| view.element == element);
            if let Some((id, _)) = pane {
                let side = (page.bounds(element)).map_or(Side::Right, |b| side_at(b, x, y));
                return Some(Target::Pane(*id, side));
            }
            let (group, _) = (strips.iter()).find(|(_, strip)| *strip == element)?;
            let count =
                (self.model).with_untracked(|model| model.group_tab_summaries(*group).len());
            Some(Target::Strip(*group, count))
        });
        let stays = |target: &Target| match *target {
            Target::Pane(pane, _) => pane == moving,
            Target::Strip(group, at) => self
                .model
                .with_untracked(|model| model.alone_in_a_tab_of(moving, group))
                .is_some_and(|own| at == own || at == own + 1),
        };
        (target.filter(|target| !stays(target)), on_label)
    }
}

/// `pane`'s tag: its name, the grip it is taken by and the button it is
/// closed with, in its top right
/// corner, clear of a terminal's text, which begins at the left. Every
/// pane has one, a pane that is all of its tab too, and it shows while the
/// pointer is over the pane or in workspace mode.
pub(super) fn tag(shell: Shell, pane: PaneId) -> El {
    el("div")
        .class(styles::TAG)
        // (What it shows by is on the tag itself, not on the pane or the
        // page: a class an element around it gains is not taken into its
        // style in every case.)
        .class_dyn(move || {
            (
                shell.mode.get().then_some(styles::IN_MODE),
                shell.moving_any().then_some(styles::AWAY),
            )
        })
        .attr("title", "Drag to move this view")
        .child(Icon::DotsSixVertical)
        .child(el("span").class(styles::TAG_NAME).text_dyn(move || {
            (shell.model.with(|model| model.pane_label(pane))).unwrap_or_default()
        }))
        // Closing the pane, or its tab when it is the tab's last, detaches
        // the session in it, as `x` does.
        .child(
            el("div")
                .class(styles::TAG_CLOSE)
                .attr("title", "Close this view")
                .text("×")
                // (Not a press that takes the view to move it.)
                .on("pointerdown", |_| {
                    prevent_default();
                    stop_propagation();
                })
                .on("click", move |_| {
                    shell.change(|model| {
                        model.exit_workspace_mode();
                        model.close_pane_or_tab(pane);
                    });
                    // (The pane this is in is off the page now: the click
                    // has no default for the document to act on it.)
                    prevent_default();
                    stop_propagation();
                }),
        )
        .on("pointerdown", move |event| {
            if let DomEventData::PointerDown(pointer) = &event.data {
                shell.begin_moving(pane, pointer.page_x(), pointer.page_y());
            }
            // (Not a press in the pane: its view keeps the focus as it
            // was, and nothing is selected.)
            prevent_default();
            stop_propagation();
        })
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
        let (over, label) = shell.target_at(page, x, y, moving.pane);
        let before = shell
            .moving
            .with_untracked(|now| now.and_then(|now| now.label));
        shell.moving.update(|now| {
            if let Some(now) = now {
                now.over = over;
                now.label = label;
            }
        });
        // A tab's label come over shows its tab, to go to its views.
        if let Some((group, index)) = label
            && before != label
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
        Some(Target::Pane(pane, side)) => shell.change(|model| {
            _ = model.move_pane_beside(moving.pane, pane, side);
        }),
        Some(Target::Strip(group, at)) => shell.change(|model| {
            _ = model.move_pane_to_new_tab_at(moving.pane, group, at);
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
