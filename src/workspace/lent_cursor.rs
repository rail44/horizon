//! The workspace's focus and cursor as an extension's view goes about
//! them (`horizon:ui/workspace`, board #95): what they are, said to the
//! extensions as they change, and the cursor moved from a view. While the
//! focus is in a view that moved it, every pane but the one the view is
//! in is veiled as in the mode, and the pane that shows the session the
//! cursor was moved to is the cursor's, its tab shown. The keys stay the
//! view's, and no pane gets the focus. A tab of the group the view is in
//! is not shown -- the view would go with the tab it is in -- and its
//! label is the cursor's in its place.

use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, With, WithUntracked};
use horizon_ui::{El, each_layout};
use horizon_workspace::types::TabGroupId;
use horizon_workspace::{Cursor, PaneId, SessionId, Workspace};

use super::Shell;
use crate::plugin_host::{CursorTarget, WorkspaceAsks};

/// A view that moved the cursor, and the session it moved it to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Held {
    view: El,
    session: SessionId,
}

/// The cursor shown: the pane the view moving it is in, if it is in one,
/// the session it was moved to, and the pane it is on, if it is on one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lent {
    from: Option<PaneId>,
    session: SessionId,
    on: Option<PaneId>,
}

impl Shell {
    /// What this shell does with the workspace's focus and cursor for an
    /// extension.
    pub(super) fn workspace_asks(&self) -> WorkspaceAsks {
        let shell = *self;
        let named = |id: &str| id.parse::<uuid::Uuid>().ok().map(SessionId::from_uuid);
        WorkspaceAsks {
            feed: self.kept().workspace_feed.clone(),
            set_cursor: std::rc::Rc::new(move |node, session| {
                let view = El(node);
                let session = session.as_deref().and_then(named);
                shell.held.update(|held| {
                    held.retain(|held| held.view != view);
                    if let Some(session) = session {
                        held.push(Held { view, session });
                    }
                })
            }),
        }
    }

    /// Keeps what extensions are told the focus and the cursor are up with
    /// the workspace.
    pub(super) fn feed_workspace(&self) {
        let shell = *self;
        Effect::new(move |_| {
            let over = shell.modal.with(Option::is_some);
            let lent = shell.lent.get();
            let mode = shell.mode.get();
            let told = |session: SessionId| session.as_uuid().to_string();
            let (focus, cursor) = shell.model.with(|model| {
                let focus = match over {
                    true => None,
                    false => model.active_session_id(),
                };
                let cursor = match lent {
                    Some(lent) => Some(CursorTarget::Session(told(lent.session))),
                    None if mode => model.workspace_cursor().map(|cursor| match cursor {
                        Cursor::Pane(pane) => (model.pane_session_id(pane))
                            .map_or(CursorTarget::Pane, |session| {
                                CursorTarget::Session(told(session))
                            }),
                        Cursor::Strip(_) => CursorTarget::Strip,
                    }),
                    None => None,
                };
                (focus.map(told), cursor)
            });
            shell.kept().workspace_feed.set(focus, cursor);
        });
    }

    /// Shows the cursor of the view with the focus, after each layout: the
    /// focus moves with no signal of its own to follow.
    pub(super) fn follow_lent_cursor(&self) {
        let shell = *self;
        each_layout(move |page| {
            // (The mode's own cursor is the one shown in the mode, but under
            // a sheet over the workspace, which has the keys.)
            let mode = shell.mode.get_untracked() && shell.modal.get_untracked().is_none();
            let lent = if mode {
                None
            } else {
                let held = shell.held.get_untracked();
                (held.iter().find(|held| page.has_focus_within(held.view))).map(|held| {
                    let kept = shell.kept();
                    let panes = kept.panes.borrow();
                    let from = (panes.iter())
                        .find(|(_, view)| page.has_focus_within(view.element))
                        .map(|(id, _)| *id);
                    let on = (shell.model)
                        .with_untracked(|model| model.pane_location_for_session(held.session));
                    Lent {
                        from,
                        session: held.session,
                        on,
                    }
                })
            };
            if shell.lent.get_untracked() != lent {
                shell.lent.set(lent);
            }
            // The tab the cursor's pane is in, shown, but in the view's own
            // group.
            if let Some(Lent {
                from, on: Some(on), ..
            }) = lent
            {
                let shown = shell.model.with_untracked(|model| {
                    let group = model.group_of(on);
                    group.is_some() && group != from.and_then(|from| model.group_of(from))
                });
                if shown {
                    let mut changed = false;
                    shell.model.update(|model| changed = model.show_tab_of(on));
                    // (The view's pane is put back in the boxes made again
                    // for the tab shown: it is given the keys again. A
                    // view over the workspace is in none of them.)
                    let active = from.is_some_and(|from| {
                        shell
                            .model
                            .with_untracked(|model| model.is_active_pane(from))
                    });
                    if changed && active {
                        shell.kept().refocus.set(true);
                    }
                }
            }
        });
    }

    /// Whether `pane` is veiled: in workspace mode, or while a view's
    /// cursor is shown, but for the pane that view is in.
    pub(super) fn veiled(&self, pane: PaneId) -> bool {
        self.mode.get() || self.lent.get().is_some_and(|lent| lent.from != Some(pane))
    }

    /// Whether a view's cursor is shown: the mode's is not, then.
    pub(super) fn lent_shown(&self) -> bool {
        self.lent.get().is_some()
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
