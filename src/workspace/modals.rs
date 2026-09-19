//! The control-surface modals' open/close lifecycles: the command
//! palette, the view chooser, the session manager, and the model picker --
//! all delegates over gpui-component's searchable `List` (`src/palette.rs`,
//! `src/view_chooser.rs`, `src/session_manager.rs`, `src/model_picker.rs`).

use gpui::*;
use gpui_component::list::{ListDelegate, ListEvent, ListState};
use gpui_component::IndexPath;
use horizon_workspace::commands::{command_entries, CommandId};
use horizon_workspace::PaneKind;

use super::WorkspaceShell;
use crate::agent::AgentSession;
use crate::model_picker::{ConfirmedModel, ModelPickerDelegate};
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

    // -- Model picker -----------------------------------------------------

    /// Opens the provider→model two-stage picker (parent task #1's Phase 2)
    /// for `session` -- the composer chip's click and the `Switch Model…`
    /// palette entry both land here via `CommandId::SwitchModel`. The
    /// provider list is fetched once per open, off the UI thread: the modal
    /// renders a loading surface until the reply lands, so a `Reload Config`
    /// between open and confirm can't show stale entries (and a stale pick
    /// fails at the daemon, never here).
    pub(super) fn open_model_picker(
        &mut self,
        session: Entity<AgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model_picker_target = Some(session);
        let list = cx.new(|cx| {
            // Deliberately no `select_first_row_on_open` here: the delegate
            // starts empty (the fetch is in flight), and the fetch completion
            // below selects the first row once rows actually exist.
            ListState::new(ModelPickerDelegate::new(), window, cx).searchable(true)
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let confirmed = list.update(cx, |list, _cx| {
                        list.delegate_mut().state_mut().confirm_at(*index)
                    });
                    match confirmed {
                        Some(confirmed) => {
                            let target = shell.model_picker_target.take();
                            shell.close_model_picker(window, cx);
                            if let Some(session) = target {
                                shell.switch_model(session, confirmed, cx);
                            }
                        }
                        // Either a provider confirm drilled into the model
                        // stage (stay open on it) or a disabled/model-less
                        // row was a no-op (stay open where it was).
                        None => {
                            list.update(cx, |list, cx| {
                                if list.delegate().state().stage()
                                    == &crate::model_picker::PickerStage::Providers
                                {
                                    return;
                                }
                                // The providers-stage query carries no
                                // meaning into the model stage: clear it and
                                // re-select the new stage's first row.
                                list.set_query("", window, cx);
                                select_first_row_on_open(list, window, cx);
                            });
                        }
                    }
                }
                ListEvent::Cancel => {
                    // Esc walks back a stage before it closes the modal.
                    let walked_back = list.update(cx, |list, cx| {
                        let walked_back = list.delegate_mut().state_mut().back();
                        if walked_back {
                            list.set_query("", window, cx);
                            select_first_row_on_open(list, window, cx);
                        }
                        walked_back
                    });
                    if !walked_back {
                        shell.model_picker_target = None;
                        shell.cancel_model_picker(window, cx);
                    }
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.model_picker = Some(list);
        self._model_picker_subscription = Some(subscription);
        cx.notify();
        self.fetch_providers(cx);
    }

    pub(super) fn close_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker = None;
        self._model_picker_subscription = None;
        self.model_picker_target = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the model picker, leaving workspace mode active when it was
    /// active before it opened (same rule as the view chooser).
    pub(super) fn cancel_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker = None;
        self._model_picker_subscription = None;
        self.model_picker_target = None;
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// One `list_providers` pull off the UI thread (it blocks up to
    /// `SYNC_REPLY_TIMEOUT`, exactly like `session_list` -- see
    /// `spawn_agent_lookup_attach`'s doc comment for why this must never run
    /// on the UI thread), delivered into the open modal. Guarded like the
    /// other async continuations: a modal closed before the reply lands, or
    /// a `Reload Agent Runtime` that swapped the daemon out from under it,
    /// drops the result instead of reviving a dead surface. An errored fetch
    /// delivers an empty list ("no providers configured") -- a retry is one
    /// close+reopen away, and the chip is still there.
    fn fetch_providers(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let runtime = handle.clone();
            let result = cx
                .background_executor()
                .spawn(async move { runtime.list_providers() })
                .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    if shell.model_picker.is_none()
                        || shell
                            .agentd
                            .as_ref()
                            .is_none_or(|current| !current.same_runtime(&handle))
                    {
                        return;
                    }
                    let providers = match result {
                        Ok(providers) => providers,
                        Err(error) => {
                            eprintln!("failed to list providers: {error}");
                            Vec::new()
                        }
                    };
                    if let Some(list) = &shell.model_picker {
                        list.update(cx, |list, cx| {
                            list.delegate_mut().state_mut().set_providers(providers);
                            // Now that rows exist, give Enter a target (the
                            // same open-time selection the other modals get).
                            select_first_row_on_open(list, window, cx);
                            cx.notify();
                        });
                    }
                });
            });
        })
        .detach();
    }

    /// Fires `SessionHub::set_session_model` for the confirmed
    /// (provider, alias) pair off the UI thread. Failure is a no-op beyond
    /// the log line: the chip keeps showing whatever the last `SessionModel`
    /// announcement said, which is exactly "the switch didn't take". On
    /// success the daemon's re-announcement drives the chip update through
    /// the existing composer projection -- no dedicated reply handling.
    fn switch_model(
        &mut self,
        session: Entity<AgentSession>,
        confirmed: ConfirmedModel,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let Some(session_id) = session.read(cx).daemon_session_id() else {
            return;
        };
        cx.spawn(async move |_this, cx| {
            let runtime = handle.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    runtime.set_session_model(session_id, confirmed.provider, confirmed.alias)
                })
                .await;
            if let Err(error) = result {
                eprintln!("failed to switch the session model: {error}");
            }
        })
        .detach();
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

    // -- Board pane ------------------------------------------------------

    /// `OpenBoard` opens the board as a native pane (`ViewKind::Board`) in a
    /// new tab, reusing the view chooser's `create_session` placement flow.
    /// The pane owns its own list/detail navigation internally (no modal
    /// overlay); see `src/board_pane.rs`.
    pub(super) fn open_board_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use horizon_workspace::ViewKind;
        self.create_session(
            PaneKind::View(ViewKind::Board),
            None,
            false,
            Placement::NewTab,
            window,
            cx,
        );
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
                    let entry = entry.filter(|entry| entry.enabled);
                    // The organizer command must capture the board under the
                    // cursor before leaving workspace mode, even when focus
                    // still belongs to another pane.
                    if !entry
                        .as_ref()
                        .is_some_and(|entry| entry.spec.id == CommandId::OpenBoardOrganizer)
                    {
                        shell.workspace.exit_workspace_mode();
                    }
                    shell.close_palette(window, cx);
                    if let Some(entry) = entry {
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
