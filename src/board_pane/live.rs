//! What one logd poke means for this pane.
//!
//! The pump itself — the subscribe thread, the forwarding loop, and the
//! handles that end both halves — lives in [`crate::board_next::live`],
//! next to the views that will replace this pane.

use super::*;

pub(super) use crate::board_next::live::LiveUpdates;

/// Starts the pane's live-update pump for `root`. Called only when a root
/// was resolved; a pane with no root gets no live updates (matching its
/// no-read empty state).
pub(super) fn start_live_updates(
    root: &std::path::Path,
    cx: &mut Context<BoardPaneView>,
) -> LiveUpdates {
    crate::board_next::live::start_live_updates(root, BoardPaneView::on_poke, cx)
}

/// What a live-update poke should refresh: the whole item list, or just the
/// currently-open detail item. The pure decision behind
/// [`BoardPaneView::on_poke`], extracted so the poke->reload mapping is
/// unit-testable without a GPUI window.
pub(super) enum PokeReloadTarget {
    /// Reload the full list (list mode).
    List,
    /// Reload just this item (detail mode).
    Item(u64),
}

/// The pure decision behind a live-update poke: `None` (list view, no item
/// open) reloads the whole list; `Some(id)` (a detail view open on `id`)
/// reloads just that item.
pub(super) fn poke_reload_target(open_item_id: Option<u64>) -> PokeReloadTarget {
    match open_item_id {
        Some(id) => PokeReloadTarget::Item(id),
        None => PokeReloadTarget::List,
    }
}

impl BoardPaneView {
    /// Reacts to one logd poke by re-reading whichever view is showing: the
    /// full list (list mode) or just the open item (detail mode -- so a
    /// comment posted from outside appears in the open thread). A poke for
    /// the user's *own* just-posted comment re-reads the same item the inline
    /// `post_comment` reload already refreshed; that one redundant file fold
    /// is the cost of staying naive (no seq tracking) -- harmless, and pokes
    /// are lossy by design so correctness can't depend on suppressing it.
    pub(super) fn on_poke(&mut self, cx: &mut Context<Self>) {
        match poke_reload_target(self.open_item_id()) {
            PokeReloadTarget::List => self.spawn_load(cx),
            PokeReloadTarget::Item(id) => {
                self.spawn_show(id, cx);
                self.spawn_load(cx);
            }
        }
    }
}
