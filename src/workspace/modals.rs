//! The control-surface modals' open/close lifecycles: the command
//! palette, the view chooser, and the session manager -- all delegates
//! over gpui-component's searchable `List` (`src/palette.rs`,
//! `src/view_chooser.rs`, `src/session_manager.rs`).

use std::path::PathBuf;

use gpui::*;
use gpui_component::list::{ListDelegate, ListEvent, ListState};
use gpui_component::IndexPath;
use horizon_board::{Item, Store};
use horizon_workspace::commands::command_entries;

use super::WorkspaceShell;
use crate::board_view::{BoardDetail, BoardDetailEvent, BoardListDelegate};
use crate::palette::PaletteDelegate;
use crate::session_manager::{subtree_session_ids, SessionManagerDelegate};
use crate::view_chooser::{Placement, ViewChooserDelegate};

/// The first row is selectable exactly when the list isn't empty — the
/// pure predicate behind [`select_first_row_on_open`], kept free of
/// `ListState`/`App` so it's unit-testable without a GPUI window.
fn first_row_to_select(items_count: usize) -> Option<IndexPath> {
    (items_count > 0).then(IndexPath::default)
}

/// The pure fallback behind [`board_root`], kept free of `WorkspaceShell`/
/// `App` so it's unit-testable without a GPUI window: the active session's
/// `workspace_root` wins; when that is absent (no active session, or a
/// terminal/resumed session with no recorded root — the common
/// terminal-only state) the shell process's own cwd stands in. Both are
/// *starting* directories — `Store::from_dir` does the worktree -> main-root
/// collapse, the same resolution `horizon board`'s `Store::from_cwd` uses —
/// so `None` only when neither is available.
fn board_root_dir(session_root: Option<PathBuf>, cwd: Option<PathBuf>) -> Option<PathBuf> {
    session_root.or(cwd)
}

/// The pure decision behind the board list's `ListEvent::Confirm` handler:
/// a confirm on a row opens the detail view *iff* both the row's item and a
/// resolvable store root are present. Extracted so the event→transition
/// mapping is unit-testable without a GPUI window (the handler in
/// [`WorkspaceShell::open_board`] just threads `item_at`/`board_root` into
/// here and calls `open_board_detail` when it yields `Some`). Returns `None`
/// on a missing item (out-of-range index) or a missing root (no session root
/// and no shell cwd — the terminal-only empty state), in which case the
/// confirm is a no-op and the modal stays on the list.
fn board_confirm_transition(item: Option<Item>, root: Option<PathBuf>) -> Option<(Item, PathBuf)> {
    let item = item?;
    let root = root?;
    Some((item, root))
}

/// Selects the first row right after a searchable `List` is constructed,
/// so a bare Enter on open runs it without arrowing down first
/// (owner report, 2026-07-13). gpui-component's `ListState` starts with
/// no selection and only re-selects a candidate in response to a query
/// change (its own `on_query_input_event`), never on construction — so
/// every palette/session-manager/view-chooser open required an arrow key
/// before Enter did anything. A no-op when the delegate starts empty:
/// `ListState::on_action_confirm` already guards Enter on an empty list.
fn select_first_row_on_open<D: ListDelegate>(
    list: &mut ListState<D>,
    window: &mut Window,
    cx: &mut Context<ListState<D>>,
) {
    if let Some(ix) = first_row_to_select(list.delegate().items_count(0, cx)) {
        list.set_selected_index(Some(ix), window, cx);
    }
}

impl WorkspaceShell {
    pub(super) fn open_view_chooser(
        &mut self,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_placement = Some(placement);
        let list = cx.new(|cx| {
            let mut list = ListState::new(ViewChooserDelegate::new(), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let choice = list.read(cx).delegate().choice_at(*index).cloned();
                    let placement = shell.pending_placement.take();
                    shell.close_view_chooser(window, cx);
                    if let (Some(choice), Some(placement)) = (choice, placement) {
                        shell.create_session(
                            choice.kind,
                            choice.role_id,
                            choice.isolate,
                            placement,
                            window,
                            cx,
                        );
                    }
                }
                ListEvent::Cancel => {
                    shell.pending_placement = None;
                    shell.cancel_view_chooser(window, cx);
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.view_chooser = Some(list);
        self._view_chooser_subscription = Some(subscription);
        cx.notify();
    }

    pub(super) fn close_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self._view_chooser_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the view chooser, leaving workspace mode active when it was
    /// active before the chooser opened.
    pub(super) fn cancel_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self._view_chooser_subscription = None;
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    pub(super) fn open_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summaries = self.workspace.session_summaries();
        let list = cx.new(|cx| {
            let mut list =
                ListState::new(SessionManagerDelegate::new(summaries), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let (summary, secondary) = {
                        let delegate = list.read(cx).delegate();
                        (
                            delegate.summary_at(*index).cloned(),
                            delegate.last_confirm_secondary(),
                        )
                    };
                    let Some(summary) = summary else {
                        return;
                    };
                    if secondary {
                        // Secondary confirm (cmd-enter / right click)
                        // terminates the session; the modal stays open
                        // on refreshed data.
                        shell.workspace.terminate_session(summary.id);
                        shell.reconcile(window, cx);
                        let sessions = shell.workspace.session_summaries();
                        list.update(cx, |list, cx| {
                            list.delegate_mut().reset(sessions);
                            cx.notify();
                        });
                        return;
                    }
                    shell.close_session_manager(window, cx);
                    if summary.attached {
                        if let Some((tab, pane)) =
                            shell.workspace.pane_location_for_session(summary.id)
                        {
                            shell.workspace.activate_pane_index(tab, pane);
                        }
                    } else {
                        shell
                            .workspace
                            .attach_existing_session_to_split_activated(summary.id, true);
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                }
                ListEvent::Cancel => shell.cancel_session_manager(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.session_manager = Some(list);
        self._session_manager_subscription = Some(subscription);
        cx.notify();
    }

    pub(super) fn close_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self._session_manager_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the session manager, leaving workspace mode active when it
    /// was active before the manager opened.
    pub(super) fn cancel_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self._session_manager_subscription = None;
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    // -- Board modal -----------------------------------------------------

    /// Resolves a starting directory for the board store: the active
    /// session's `workspace_root` when one is known, else the shell
    /// process's own cwd (the GUI is normally launched from a repo
    /// checkout). Either is a *starting* directory — `Store::from_dir` does
    /// the worktree -> main-root collapse, the same resolution `horizon
    /// board`'s `Store::from_cwd` uses. The cwd fallback covers the common
    /// terminal-only state, where the active session is a terminal (or a
    /// resumed agent) with no recorded `workspace_root`; `None` only when
    /// neither is available, leaving the modal's empty state.
    fn board_root(&self) -> Option<PathBuf> {
        let session_root = self.workspace.active_session_id().and_then(|id| {
            self.workspace
                .session_workspace_root(id)
                .map(|path| path.to_path_buf())
        });
        board_root_dir(session_root, std::env::current_dir().ok())
    }

    pub(super) fn open_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.board_root();
        let list = cx.new(|cx| {
            let mut list = ListState::new(BoardListDelegate::new(), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let item = list.read(cx).delegate().item_at(*index).cloned();
                    let root = shell.board_root();
                    if let Some((item, root)) = board_confirm_transition(item, root) {
                        shell.board = None;
                        shell._board_subscription = None;
                        shell.open_board_detail(item, root, window, cx);
                    }
                }
                ListEvent::Cancel => shell.cancel_board(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.board = Some(list);
        self._board_subscription = Some(subscription);
        cx.notify();
        match root {
            Some(root) => {
                // Load off the UI thread: `Store::from_dir` runs `git rev-parse`
                // and `list` reads+folds the event log -- both blocking.
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            Store::from_dir(&root)
                                .and_then(|store| store.list(None))
                                .map(|result| result.items)
                        })
                        .await;
                    let _ = this.update(cx, |shell, cx| {
                        if let Some(list) = shell.board.as_ref() {
                            list.update(cx, |list, cx| {
                                list.delegate_mut().set_loaded(result.unwrap_or_default());
                                cx.notify();
                            });
                        }
                    });
                })
                .detach();
            }
            None => {
                // Neither a session root nor the shell cwd yielded a starting
                // directory: leave the empty (non-loading) state rather than a
                // perpetual "Loading…".
                if let Some(list) = self.board.as_ref() {
                    list.update(cx, |list, cx| {
                        list.delegate_mut().set_loaded(Vec::new());
                        cx.notify();
                    });
                }
            }
        }
    }

    pub(super) fn cancel_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.board = None;
        self._board_subscription = None;
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn open_board_detail(
        &mut self,
        item: Item,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = cx.new(|cx| BoardDetail::new(item, root, window, cx));
        let subscription = cx.subscribe_in(
            &detail,
            window,
            |shell, _detail, event: &BoardDetailEvent, window, cx| match event {
                BoardDetailEvent::Back => shell.back_from_board_detail(window, cx),
            },
        );
        window.focus(&detail.read(cx).input_focus_handle(cx), cx);
        self.board_detail = Some(detail);
        self._board_detail_subscription = Some(subscription);
        cx.notify();
    }

    fn close_board_detail(&mut self, cx: &mut Context<Self>) {
        self.board_detail = None;
        self._board_detail_subscription = None;
        cx.notify();
    }

    /// Back from the detail to the (re-read) list: close the detail and re-open
    /// the board, which re-resolves the root and re-reads the store so a
    /// just-posted comment's bump to the row's count/updated is visible.
    pub(super) fn back_from_board_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_board_detail(cx);
        self.open_board(window, cx);
    }

    /// `OpenSessionDirectory` (`docs/session-relationship-design.md`
    /// decision 4b): opens a new terminal pinned to the session manager's
    /// currently *selected* row's directory -- generalizing decision 4a's
    /// active-session-only v1 (`CommandId::OpenTerminalInSessionDirectory`)
    /// to an arbitrary row. A no-op if nothing is selected or the selected
    /// row's `workspace_root` isn't known (every terminal session today,
    /// plus a resumed agent session -- same enablement rule as the active-
    /// session command).
    pub(super) fn open_selected_session_directory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(manager) = self.session_manager.clone() else {
            return;
        };
        let workspace_root = manager.read(cx).selected_index().and_then(|index| {
            manager
                .read(cx)
                .delegate()
                .summary_at(index)
                .and_then(|summary| summary.workspace_root.clone())
        });
        if let Some(workspace_root) = workspace_root {
            self.open_terminal_in_directory(workspace_root, window, cx);
        }
    }

    /// `TerminateSessionSubtree` (decision 5's explicit, more-destructive-
    /// than-plain-terminate opt-in): terminates the session manager's
    /// currently *selected* row and every descendant, leaving unrelated
    /// sessions (including the row's own ancestors) untouched. A no-op
    /// unless the selected row actually has children -- this must never
    /// substitute for the plain per-session terminate a leaf row already
    /// gets from secondary confirm. Each terminated session keeps its own
    /// independent cleanup semantics (clean worktree removed, dirty kept,
    /// branch never deleted; design decision 5) -- `Workspace::
    /// terminate_session` doesn't care about traversal order, so
    /// `subtree_session_ids`'s order is used as-is.
    pub(super) fn terminate_selected_session_subtree(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(manager) = self.session_manager.clone() else {
            return;
        };
        let target = manager.read(cx).selected_index().and_then(|index| {
            manager
                .read(cx)
                .delegate()
                .row_at(index)
                .filter(|row| row.has_children)
                .map(|row| row.summary.id)
        });
        let Some(target) = target else {
            return;
        };
        let sessions = self.workspace.session_summaries();
        for session_id in subtree_session_ids(&sessions, target) {
            self.workspace.terminate_session(session_id);
        }
        self.reconcile(window, cx);
        let sessions = self.workspace.session_summaries();
        manager.update(cx, |list, cx| {
            list.delegate_mut().reset(sessions);
            cx.notify();
        });
    }

    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Palette is a transient overlay: workspace mode itself stays active
        // so cancelling it (Esc / click-outside) simply returns to the same
        // cursor state. Only confirming a command exits the mode first.
        let entries = command_entries(self.command_state_with(cx));
        let list = cx.new(|cx| {
            let mut list =
                ListState::new(PaletteDelegate::new(entries), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let entry = list.read(cx).delegate().entry_at(*index).cloned();
                    // Confirming a palette command exits workspace mode:
                    // creating commands dive, non-creating commands run in
                    // normal mode. Cancel (Esc) keeps the mode instead.
                    shell.workspace.exit_workspace_mode();
                    shell.close_palette(window, cx);
                    if let Some(entry) = entry.filter(|entry| entry.enabled) {
                        shell.execute(entry.spec.id, window, cx);
                    }
                }
                ListEvent::Cancel => shell.cancel_palette(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.palette = Some(list);
        self._palette_subscription = Some(subscription);
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_subscription = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the palette, leaving workspace mode active when it was
    /// active before the palette opened. The modal's own focus is released
    /// back to the shell root so mode keys keep dispatching.
    pub(super) fn cancel_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_subscription = None;
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gpui_component::IndexPath;

    use super::{board_confirm_transition, board_root_dir, first_row_to_select};

    #[test]
    fn first_row_to_select_is_the_default_index_when_the_list_is_nonempty() {
        assert_eq!(first_row_to_select(1), Some(IndexPath::default()));
        assert_eq!(first_row_to_select(5), Some(IndexPath::default()));
    }

    #[test]
    fn first_row_to_select_is_none_when_the_list_is_empty() {
        assert_eq!(first_row_to_select(0), None);
    }

    #[test]
    fn board_root_dir_falls_back_to_cwd_when_no_session_root() {
        // Terminal-only workspace: the active session is a terminal (or a
        // resumed agent) with no recorded `workspace_root`, so `session_root`
        // is None -- the bug condition that used to force "No board items".
        // The shell cwd stands in so `Store::from_dir` still gets a chance to
        // resolve the store (worktree -> main root).
        let cwd = PathBuf::from("/repo/worktree");
        assert_eq!(board_root_dir(None, Some(cwd.clone())), Some(cwd));
    }

    #[test]
    fn board_root_dir_prefers_the_session_root_over_cwd() {
        let session_root = PathBuf::from("/agent/worktree");
        let cwd = PathBuf::from("/shell/cwd");
        assert_eq!(
            board_root_dir(Some(session_root.clone()), Some(cwd)),
            Some(session_root)
        );
    }

    #[test]
    fn board_root_dir_is_none_when_neither_available() {
        // No session root and an unreadable cwd: empty state, as before.
        assert_eq!(board_root_dir(None, None), None);
    }

    fn item(id: u64, title: &str) -> horizon_board::Item {
        horizon_board::Item {
            id,
            title: title.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn board_confirm_transition_opens_detail_when_item_and_root_present() {
        // The normal case: a row is confirmed (Enter or click — both emit
        // `ListEvent::Confirm`), the item exists and the store root resolves,
        // so the modal opens the detail view for that item.
        let item = item(7, "Fix modal click bug");
        let root = PathBuf::from("/repo/worktree");
        assert_eq!(
            board_confirm_transition(Some(item.clone()), Some(root.clone())),
            Some((item, root))
        );
    }

    #[test]
    fn board_confirm_transition_is_noop_when_item_missing() {
        // An out-of-range index (e.g. confirm on an empty list, or a stale
        // index after a filter narrowed the list): no item, so no detail —
        // the modal stays on the list rather than opening an empty detail.
        let root = PathBuf::from("/repo/worktree");
        assert_eq!(board_confirm_transition(None, Some(root)), None);
    }

    #[test]
    fn board_confirm_transition_is_noop_when_root_missing() {
        // Terminal-only state with an unreadable cwd: the item is present but
        // no store root can be resolved, so the confirm is a no-op (the modal
        // can't open a detail it has no store to read from).
        let item = item(1, "Task");
        assert_eq!(board_confirm_transition(Some(item), None), None);
    }

    #[test]
    fn board_confirm_transition_is_noop_when_both_missing() {
        assert_eq!(board_confirm_transition(None, None), None);
    }
}
