//! The control-surface modals' open/close lifecycles: the command
//! palette, the view chooser, and the session manager -- all delegates
//! over gpui-component's searchable `List` (`src/palette.rs`,
//! `src/view_chooser.rs`, `src/session_manager.rs`).

use gpui::*;
use gpui_component::list::{ListDelegate, ListEvent, ListState};
use gpui_component::IndexPath;
use horizon_workspace::commands::command_entries;

use super::WorkspaceShell;
use crate::palette::PaletteDelegate;
use crate::session_manager::{subtree_session_ids, SessionManagerDelegate};
use crate::view_chooser::{Placement, ViewChooserDelegate};

/// The first row is selectable exactly when the list isn't empty — the
/// pure predicate behind [`select_first_row_on_open`], kept free of
/// `ListState`/`App` so it's unit-testable without a GPUI window.
fn first_row_to_select(items_count: usize) -> Option<IndexPath> {
    (items_count > 0).then(IndexPath::default)
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
    /// Snapshots workspace mode's dim/cursor pattern into `scrim_freeze`
    /// right before a modal-opening handler exits the mode -- see
    /// `render::effective_scrim_pattern`'s doc comment for why this is
    /// necessary (the mode's own key bindings must detach before the
    /// modal's `List` takes focus, which erases `cursor_pane_id`'s
    /// target). `render::render_node` consumes this for both the scrim
    /// and the cursor-pane border while a modal is open. Must be called
    /// before `Workspace::exit_workspace_mode`, not after.
    fn freeze_scrim_before_modal_exit(&mut self) {
        self.scrim_freeze = if self.workspace.is_workspace_mode_active() {
            self.workspace.cursor_pane_id()
        } else {
            None
        };
    }

    pub(super) fn open_view_chooser(
        &mut self,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
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
                    shell.close_view_chooser(window, cx);
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
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn open_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
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
                ListEvent::Cancel => shell.close_session_manager(window, cx),
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
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
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
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
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
                    shell.close_palette(window, cx);
                    if let Some(entry) = entry.filter(|entry| entry.enabled) {
                        shell.execute(entry.spec.id, window, cx);
                    }
                }
                ListEvent::Cancel => shell.close_palette(window, cx),
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
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui_component::IndexPath;

    use super::first_row_to_select;

    #[test]
    fn first_row_to_select_is_the_default_index_when_the_list_is_nonempty() {
        assert_eq!(first_row_to_select(1), Some(IndexPath::default()));
        assert_eq!(first_row_to_select(5), Some(IndexPath::default()));
    }

    #[test]
    fn first_row_to_select_is_none_when_the_list_is_empty() {
        assert_eq!(first_row_to_select(0), None);
    }
}
