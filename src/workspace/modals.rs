//! The control-surface modals' open/close lifecycles: the command
//! palette, the view chooser, the session manager, and the model picker --
//! all delegates over gpui-component's searchable `List` (`src/palette.rs`,
//! `src/view_chooser.rs`, `src/session_manager.rs`, `src/model_picker.rs`).

use gpui::*;
use gpui_component::list::{ListDelegate, ListEvent, ListState};
use gpui_component::IndexPath;
use horizon_workspace::commands::{command_entries, CommandId};
use horizon_workspace::PaneKind;

mod owner;
pub(super) use owner::ListModal;

use super::WorkspaceShell;
use crate::agent::AgentSession;
use crate::model_picker::{ConfirmedModel, ModelPickerDelegate};
use crate::palette::PaletteDelegate;
use crate::session_manager::{subtree_session_ids, SessionManagerDelegate};
use crate::view_chooser::{Placement, ViewChooserDelegate};

enum ModelPickerQuery {
    Providers,
    Models { provider: usize, name: String },
}

enum ModelPickerReply {
    Providers(Vec<horizon_agent::wire::ProviderSummary>),
    Models { provider: usize, ids: Vec<String> },
}

impl ModelPickerQuery {
    fn fetch(self, runtime: &crate::runtime::AgentdHandle) -> ModelPickerReply {
        match self {
            Self::Providers => {
                ModelPickerReply::Providers(runtime.list_providers().unwrap_or_else(|error| {
                    eprintln!("failed to list providers: {error}");
                    Vec::new()
                }))
            }
            Self::Models { provider, name } => ModelPickerReply::Models {
                provider,
                ids: runtime.list_provider_models(name).unwrap_or_else(|error| {
                    eprintln!("failed to list models for provider {provider}: {error}");
                    Vec::new()
                }),
            },
        }
    }
}

impl ModelPickerReply {
    /// Cache off-stage model replies without disturbing the current selection.
    fn apply(self, state: &mut crate::model_picker::PickerState) -> bool {
        match self {
            Self::Providers(providers) => {
                state.set_providers(providers);
                true
            }
            Self::Models { provider, ids } => {
                let current =
                    state.stage() == &crate::model_picker::PickerStage::Models { provider };
                state.set_live_models(provider, ids);
                current
            }
        }
    }
}

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
    /// Cancellation preserves workspace mode and its cursor; confirmation
    /// instead restores focus directly to the active pane.
    fn restore_focus_after_modal_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.is_workspace_mode_active() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    pub(super) fn open_view_chooser(
        &mut self,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    let placement = shell.view_chooser.take().map(|modal| modal.target);
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
                    shell.cancel_view_chooser(window, cx);
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.view_chooser = Some(ListModal::new(list, placement, subscription));
        cx.notify();
    }

    pub(super) fn close_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the view chooser, leaving workspace mode active when it was
    /// active before the chooser opened.
    pub(super) fn cancel_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self.restore_focus_after_modal_cancel(window, cx);
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
                            let target = shell.model_picker.take().map(|modal| modal.target);
                            shell.close_model_picker(window, cx);
                            if let Some(session) = target {
                                shell.switch_model(session, confirmed, cx);
                            }
                        }
                        // Either a provider confirm drilled into the model
                        // stage (ask the daemon for its live /models listing,
                        // once, and stay open on it) or a disabled provider row
                        // was a no-op (stay open where it was).
                        None => {
                            let stage = list.read(cx).delegate().state().stage().clone();
                            let crate::model_picker::PickerStage::Models { provider } = stage
                            else {
                                return;
                            };
                            let needs_fetch = list.update(cx, |list, cx| {
                                // The providers-stage query carries no meaning
                                // into the model stage: clear it and re-select
                                // the new stage's first row.
                                list.set_query("", window, cx);
                                select_first_row_on_open(list, window, cx);
                                list.delegate_mut().state_mut().begin_live_load(provider)
                            });
                            if needs_fetch {
                                shell.fetch_provider_models(provider, cx);
                            }
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
                        shell.cancel_model_picker(window, cx);
                    }
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.model_picker = Some(ListModal::new(list, session, subscription));
        cx.notify();
        self.fetch_providers(cx);
    }

    pub(super) fn close_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the model picker, leaving workspace mode active when it was
    /// active before it opened (same rule as the view chooser).
    pub(super) fn cancel_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_picker = None;
        self.restore_focus_after_modal_cancel(window, cx);
    }

    fn fetch_providers(&mut self, cx: &mut Context<Self>) {
        self.fetch_picker(ModelPickerQuery::Providers, cx);
    }

    fn fetch_provider_models(&mut self, provider: usize, cx: &mut Context<Self>) {
        let Some(modal) = &self.model_picker else {
            return;
        };
        let Some(name) = modal
            .list
            .read(cx)
            .delegate()
            .state()
            .providers()
            .get(provider)
            .map(|entry| entry.name.clone())
        else {
            return;
        };
        self.fetch_picker(ModelPickerQuery::Models { provider, name }, cx);
    }

    /// The continuation belongs to this opening of the picker. Closing it
    /// cancels delivery, and the weak list reference can never address a
    /// reopened picker. A blocking daemon request already underway may finish
    /// in the background; it has no authority to update a replacement modal.
    fn fetch_picker(&mut self, query: ModelPickerQuery, cx: &mut Context<Self>) {
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let Some(modal) = &mut self.model_picker else {
            return;
        };
        let this = cx.weak_entity();
        let runtime = handle.clone();
        let reply = cx
            .background_executor()
            .spawn(async move { query.fetch(&runtime) });
        modal.receive(
            self.window,
            reply,
            move |reply, list, window, cx| {
                let current_runtime = this
                    .read_with(cx, |shell, _| {
                        shell
                            .agentd
                            .as_ref()
                            .is_some_and(|current| current.same_runtime(&handle))
                    })
                    .unwrap_or(false);
                if !current_runtime {
                    return;
                }
                if reply.apply(list.delegate_mut().state_mut()) {
                    select_first_row_on_open(list, window, cx);
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Fires `SessionHub::set_session_model` for the confirmed
    /// (provider, model id) pair off the UI thread. Failure is a no-op beyond
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
            eprintln!("model switch skipped: the session has no live attachment (mid-reload?)");
            return;
        };
        cx.spawn(async move |_this, cx| {
            let runtime = handle.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    runtime.set_session_model(session_id, confirmed.provider, confirmed.model)
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
        self.session_manager = Some(ListModal::new(list, (), subscription));
        cx.notify();
    }

    pub(super) fn close_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the session manager, leaving workspace mode active when it
    /// was active before the manager opened.
    pub(super) fn cancel_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self.restore_focus_after_modal_cancel(window, cx);
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
        let Some(manager) = self
            .session_manager
            .as_ref()
            .map(|modal| modal.list.clone())
        else {
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
        let Some(manager) = self
            .session_manager
            .as_ref()
            .map(|modal| modal.list.clone())
        else {
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
        self.palette = Some(ListModal::new(list, (), subscription));
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Cancels the palette, leaving workspace mode active when it was
    /// active before the palette opened. The modal's own focus is released
    /// back to the shell root so mode keys keep dispatching.
    pub(super) fn cancel_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.restore_focus_after_modal_cancel(window, cx);
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
    #[test]
    fn an_off_stage_model_reply_keeps_the_current_stage() {
        use super::ModelPickerReply;
        use crate::model_picker::{PickerStage, PickerState};
        use horizon_agent::wire::ProviderSummary;
        let mut state = PickerState::new();
        state.set_providers(
            ["first", "second"]
                .map(|name| ProviderSummary {
                    name: name.into(),
                    base_url: None,
                    api_key_env: String::new(),
                    default_model: None,
                    available: true,
                    default: false,
                })
                .into(),
        );
        state.confirm_at(IndexPath::new(0));
        assert!(state.begin_live_load(0));
        state.back();
        state.confirm_at(IndexPath::new(1));
        assert!(state.begin_live_load(1));
        assert!(!ModelPickerReply::Models {
            provider: 0,
            ids: vec!["model-a".into()]
        }
        .apply(&mut state));
        assert_eq!(state.stage(), &PickerStage::Models { provider: 1 });
        assert!(state.items().is_empty());
        state.back();
        state.confirm_at(IndexPath::new(0));
        assert!(
            !state.begin_live_load(0),
            "the off-stage answer is still cached"
        );
        assert_eq!(
            state.confirm_at(IndexPath::new(0)).unwrap().model,
            "model-a"
        );
    }
}
