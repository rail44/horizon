//! Workspace mode's cursor, moved from an extension's view
//! (`horizon:ui/workspace`'s `cursor`, board #95): while the focus is in
//! a view that holds one, every pane but the one the view is in is veiled
//! as in the mode, and the pane that shows the session the cursor was
//! moved to is the cursor's -- or, where that pane's tab is not shown, the
//! tab's label is. The keys stay the view's, and nothing else changes: no
//! tab is shown, no pane gets the focus.

use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_ui::{El, each_layout};
use horizon_workspace::types::TabGroupId;
use horizon_workspace::{PaneId, SessionId, Workspace};

use super::Shell;
use crate::plugin_host::CursorAsks;

/// A view that holds a cursor, and the session it was moved to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Held {
    view: El,
    session: Option<SessionId>,
}

/// The cursor shown: the pane the view moving it is in, if it is in one,
/// and the pane it is on, if it is on one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lent {
    from: Option<PaneId>,
    on: Option<PaneId>,
}

impl Shell {
    /// What this shell does with workspace mode's cursor for an extension.
    pub(super) fn cursor_asks(&self) -> CursorAsks {
        let shell = *self;
        let named = |id: &str| id.parse::<uuid::Uuid>().ok().map(SessionId::from_uuid);
        CursorAsks {
            hold: std::rc::Rc::new(move |node| {
                (shell.held).update(|held| {
                    held.push(Held {
                        view: El(node),
                        session: None,
                    })
                })
            }),
            move_to: std::rc::Rc::new(move |node, session| {
                let session = session.as_deref().and_then(named);
                shell.held.update(|held| {
                    for held in held.iter_mut().filter(|held| held.view == El(node)) {
                        held.session = session;
                    }
                })
            }),
            release: std::rc::Rc::new(move |node| {
                (shell.held).update(|held| held.retain(|held| held.view != El(node)))
            }),
        }
    }

    /// Shows the cursor of the view with the focus, after each layout: the
    /// focus moves with no signal of its own to follow.
    pub(super) fn follow_lent_cursor(&self) {
        let shell = *self;
        each_layout(move |page| {
            let lent = if shell.mode.get_untracked() {
                None
            } else {
                let held = shell.held.get_untracked();
                (held.iter().find(|held| page.has_focus_within(held.view))).map(|held| {
                    let kept = shell.kept();
                    let panes = kept.panes.borrow();
                    let from = (panes.iter())
                        .find(|(_, view)| page.has_focus_within(view.element))
                        .map(|(id, _)| *id);
                    let on = held.session.and_then(|session| {
                        (shell.model)
                            .with_untracked(|model| model.pane_location_for_session(session))
                    });
                    Lent { from, on }
                })
            };
            if shell.lent.get_untracked() != lent {
                shell.lent.set(lent);
            }
        });
    }

    /// Whether `pane` is veiled: in workspace mode, or while a view's
    /// cursor is shown, but for the pane that view is in.
    pub(super) fn veiled(&self, pane: PaneId) -> bool {
        self.mode.get() || self.lent.get().is_some_and(|lent| lent.from != Some(pane))
    }

    /// Whether a view's cursor is on `pane`.
    pub(super) fn lent_cursor_on(&self, pane: PaneId) -> bool {
        self.lent.get().is_some_and(|lent| lent.on == Some(pane))
    }

    /// Whether a view's cursor is on the label of the tab at `index` in
    /// `group`: the pane it is on is in that tab, which is not shown.
    pub(super) fn lent_cursor_on_tab(&self, group: TabGroupId, index: usize) -> bool {
        let Some(pane) = self.lent.get().and_then(|lent| lent.on) else {
            return false;
        };
        self.model.with(|model: &Workspace| {
            let tab = model.tab_in(group, index);
            let shown = (model.tab_groups().into_iter())
                .find(|of| of.id == group)
                .map(|of| of.shown);
            tab.is_some() && tab == model.tab_of(pane) && tab != shown
        })
    }
}
