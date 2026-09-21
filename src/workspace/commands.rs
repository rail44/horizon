//! The command-model dispatch point (`execute`/`execute_control_plane`)
//! plus the session-targeted `control_plane_*` family the CLI control
//! plane drives (everything but `control_plane_new_session`, which pairs
//! with `create_session` in `session_lifecycle` instead -- see that
//! module's doc comment).

use gpui::*;
use horizon_workspace::commands::{CommandId, CommandState};
use horizon_workspace::types::SessionKind;
use horizon_workspace::{CloseCursorOutcome, SessionId, Workspace};

use super::session_lifecycle::{daemon_summary_for, DaemonAgentAdoption, ExistingAgentEntity};
use super::{CachedPaneLeaf, CompositePane, PaneView, WorkspaceShell};
use crate::agent::AgentSession;
use crate::theme;
use crate::view_chooser::Placement;

/// Removes every terminal session from the model ahead of restarting
/// `horizon-terminald` — the daemon that owns their PTYs is about to be
/// replaced, so those sessions genuinely end
/// (`docs/terminald-split-design.md` decision 3). Agent sessions are
/// untouched: they live in the other daemon.
///
/// This used to run on the *session*-runtime reload path, where it was pure
/// collateral damage — the daemon restart killed the PTYs whether the user
/// wanted it or not. Since the split, `Reload Agent Runtime` leaves
/// terminals alone entirely and this belongs to the one command that is
/// explicitly destructive.
fn prepare_workspace_for_terminal_runtime_reload(workspace: &mut Workspace) {
    let terminals = workspace
        .session_summaries()
        .into_iter()
        .filter(|summary| summary.kind == SessionKind::Terminal)
        .map(|summary| summary.id)
        .collect::<Vec<_>>();
    for session_id in terminals {
        workspace.terminate_session(session_id);
    }
}

fn command_blocked_by_restore(restoring: bool, failed: bool, id: CommandId) -> bool {
    restoring
        && !(failed
            && matches!(
                id,
                CommandId::ReloadAgentRuntime | CommandId::ReloadTerminalRuntime
            ))
}

/// Binding writes precede asynchronous daemon installation. Retry only this
/// board's missing bindings; never start a session or adopt unrelated sessions.
fn load_board_summaries(
    mut list: impl FnMut() -> Result<Vec<horizon_agent::wire::SessionSummary>, String>,
    needed: &std::collections::HashSet<SessionId>,
) -> Result<Vec<horizon_agent::wire::SessionSummary>, String> {
    for attempt in 0..20 {
        let summaries = list()?;
        if attempt == 19
            || needed.iter().all(|id| {
                summaries
                    .iter()
                    .any(|summary| summary.session_id.as_uuid() == id.as_uuid())
            })
        {
            return Ok(summaries);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    unreachable!("last attempt returns")
}

fn board_session_still_requested(was_registered: bool, is_registered: bool) -> bool {
    // Removing an already-known session while lookup was running is an
    // explicit termination; a stale summary must not add it back.
    !was_registered || is_registered
}

impl WorkspaceShell {
    /// Board-side adoption: `refresh_board_sessions`/
    /// `open_board_organizer`/`open_board_task_session` all funnel through
    /// this thin wrapper. The full adoption sequence -- registration, the
    /// daemon-authoritative root/parent refresh, and the `AgentSession`
    /// entity -- lives in
    /// [`WorkspaceShell::adopt_daemon_agent_session`].
    fn adopt_board_session(
        &mut self,
        handle: &crate::runtime::AgentdHandle,
        summary: horizon_agent::wire::SessionSummary,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.adopt_daemon_agent_session(
            DaemonAgentAdoption::from(&summary),
            ExistingAgentEntity::Reattach,
            || handle.attach_session(summary.session_id),
            cx,
        )
        .map(|_| ())
    }

    pub(super) fn sync_board_session_states(&self, cx: &mut Context<Self>) {
        for pane in self.panes.values() {
            if let PaneView::Cached(CachedPaneLeaf::Board(view)) = pane {
                view.update(cx, |view, cx| {
                    view.observe_sessions(&self.agent_sessions, cx)
                });
            }
        }
    }

    pub(super) fn refresh_board_sessions(
        &self,
        view: Entity<crate::board_pane::BoardPaneView>,
        sessions: Vec<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let needed = sessions
            .iter()
            .copied()
            .filter(|id| {
                self.workspace.session_pane_kind(*id).is_none()
                    || self
                        .agent_sessions
                        .get(id)
                        .is_none_or(|session| session.read(cx).runtime_unreachable())
            })
            .collect::<std::collections::HashSet<_>>();
        if needed.is_empty() || self.agentd.is_none() {
            view.update(cx, |view, cx| {
                view.finish_inventory_refresh(&sessions);
                view.observe_sessions(&self.agent_sessions, cx);
            });
            return;
        }
        view.update(cx, |view, cx| {
            view.observe_sessions(&self.agent_sessions, cx)
        });
        let registered_at_request = needed
            .iter()
            .copied()
            .filter(|id| self.workspace.session_pane_kind(*id).is_some())
            .collect::<std::collections::HashSet<_>>();
        let handle = self.agentd.clone().expect("checked above");
        let epoch = view.read(cx).navigation_epoch();
        cx.spawn(async move |this, cx| {
            let request = handle.clone();
            let requested = needed.clone();
            let result = cx
                .background_executor()
                .spawn(async move { load_board_summaries(|| request.session_list(), &requested) })
                .await;
            let _ = this.update(cx, |shell, cx| {
                view.update(cx, |view, _| view.finish_inventory_refresh(&sessions));
                if shell.restoring_workspace
                    || shell
                        .agentd
                        .as_ref()
                        .is_none_or(|current| !current.same_runtime(&handle))
                {
                    return;
                }
                let result = result.and_then(|summaries| {
                    for summary in summaries {
                        let id = SessionId::from_uuid(summary.session_id.as_uuid());
                        if needed.contains(&id)
                            && board_session_still_requested(
                                registered_at_request.contains(&id),
                                shell.workspace.session_pane_kind(id).is_some(),
                            )
                        {
                            shell.adopt_board_session(&handle, summary, cx)?;
                        }
                    }
                    Ok(())
                });
                shell.sync_board_session_states(cx);
                if let Err(error) = result {
                    if view.read(cx).navigation_epoch() == epoch {
                        view.update(cx, |view, cx| view.set_error(error, cx));
                    }
                }
                shell.persist_workspace();
                cx.notify();
            });
        })
        .detach();
    }

    fn open_board_organizer(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active_board_pane() else {
            return;
        };
        let Some(root) = view.read(cx).root() else {
            return;
        };
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let origin = self.workspace.cursor_pane_id();
        let registered_at_request = self
            .workspace
            .session_summaries()
            .into_iter()
            .map(|summary| summary.id)
            .collect::<std::collections::HashSet<_>>();
        self.workspace.commit_workspace_mode();
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let request = handle.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let id = request.ensure_board_organizer(root)?;
                    let needed = [SessionId::from_uuid(id.as_uuid())].into_iter().collect();
                    load_board_summaries(|| request.session_list(), &needed)?
                        .into_iter()
                        .find(|summary| summary.session_id == id)
                        .ok_or_else(|| "The board organizer is not available yet".to_string())
                })
                .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    if shell.restoring_workspace
                        || shell
                            .agentd
                            .as_ref()
                            .is_none_or(|current| !current.same_runtime(&handle))
                    {
                        return;
                    }
                    let result = result.and_then(|summary| {
                        let id = SessionId::from_uuid(summary.session_id.as_uuid());
                        if !board_session_still_requested(
                            registered_at_request.contains(&id),
                            shell.workspace.session_pane_kind(id).is_some(),
                        ) {
                            return Ok(());
                        }
                        shell.adopt_board_session(&handle, summary, cx)?;
                        // Keep the session accessible in Manage Sessions if
                        // the owner navigated away while it was starting.
                        if shell.workspace.cursor_pane_id() != origin {
                            shell.persist_workspace();
                            return Ok(());
                        }
                        shell.workspace.commit_workspace_mode();
                        if let Some((tab, pane)) = shell.workspace.pane_location_for_session(id) {
                            shell.workspace.activate_pane_index(tab, pane);
                            shell.focus_active(window, cx);
                            cx.notify();
                            Ok(())
                        } else {
                            shell.attach_known_session(id, true, window, cx)
                        }
                    });
                    if let Err(error) = result {
                        view.update(cx, |view, cx| view.set_error(error, cx));
                    }
                });
            });
        })
        .detach();
    }

    fn open_board_task_session(&self, cx: &mut Context<Self>) {
        let Some(view) = self.active_board_pane() else {
            return;
        };
        let Some(session_id) = view.read(cx).task_session() else {
            return;
        };
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let navigation_epoch = view.read(cx).navigation_epoch();
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let list_handle = handle.clone();
            let result = cx
                .background_executor()
                .spawn(async move { list_handle.session_list() })
                .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    if view.read(cx).navigation_epoch() != navigation_epoch
                        || shell.restoring_workspace
                        || shell
                            .agentd
                            .as_ref()
                            .is_none_or(|h| !h.same_runtime(&handle))
                    {
                        return;
                    }
                    let summary = result.ok().and_then(|items| {
                        items
                            .into_iter()
                            .find(|s| s.session_id.as_uuid() == session_id.as_uuid())
                    });
                    let Some(summary) = summary else {
                        view.update(cx, |view, cx| {
                            view.set_error(
                                "The session is not available in the current agent runtime".into(),
                                cx,
                            )
                        });
                        return;
                    };
                    if let Err(error) = shell.adopt_board_session(&handle, summary, cx) {
                        view.update(cx, |view, cx| view.set_error(error, cx));
                        return;
                    }
                    if let Err(error) = shell.attach_known_session(session_id, true, window, cx) {
                        view.update(cx, |view, cx| view.set_error(error, cx));
                    }
                });
            });
        })
        .detach();
    }

    /// The active pane's agent session, when it is an agent pane.
    fn active_agent_session(&self) -> Option<Entity<AgentSession>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let session_id = self.workspace.agent_session_id(pane_id)?;
        self.agent_sessions.get(&session_id).cloned()
    }

    /// The active board pane, when the active pane is a board pane. Board
    /// panes are session-less, so this is a direct `panes` lookup (no
    /// session-id indirection, unlike `active_agent_session`).
    fn active_board_pane(&self) -> Option<Entity<crate::board_pane::BoardPaneView>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        match self.panes.get(&pane_id)? {
            PaneView::Cached(CachedPaneLeaf::Board(view)) => Some(view.clone()),
            _ => None,
        }
    }

    /// Re-pushes the live theme's terminal color scheme to every running
    /// terminal session, so OSC 10/11/12 query replies reflect a live
    /// theme apply instead of each session's spawn-time snapshot. Called
    /// right after `Reload Config` swaps the live scheme
    /// (`theme::reload_from`); the theme settings view's own live-apply
    /// path (`src/theme_settings/mod.rs`) calls the same
    /// `TerminaldHandle` method directly, since it holds its own clone.
    fn broadcast_terminal_color_scheme(&self) {
        if let Some(terminald) = &self.terminald {
            terminald.broadcast_terminal_color_scheme(theme::terminal_color_scheme());
        }
    }

    /// The M3 dispatch point: every surface (palette, keybindings, and
    /// later the control plane) funnels through here — the GPUI
    /// counterpart of the Floem shell's `execute_command`.
    pub(super) fn execute(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        if command_blocked_by_restore(self.restoring_workspace, self.workspace_restore_failed, id) {
            return;
        }
        match id {
            CommandId::SplitRight => self.open_view_chooser(Placement::SplitRight, window, cx),
            CommandId::SplitDown => self.open_view_chooser(Placement::SplitDown, window, cx),
            CommandId::NewTab => self.open_view_chooser(Placement::NewTab, window, cx),
            CommandId::FocusNextPane => {
                self.workspace.focus_next();
                self.focus_active(window, cx);
                cx.notify();
            }
            CommandId::CloseActivePane => self.close_pane(window, cx),
            CommandId::CloseActiveTab => {
                self.workspace.exit_workspace_mode();
                self.workspace.close_active_tab();
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateActiveSession => {
                // Terminate before exiting workspace mode: `exit_workspace_
                // mode` clears `workspace_mode_cursor`, and
                // `terminate_active_session` resolves its target through
                // `cursor_session_id` (the cursor pane, falling back to the
                // focused pane outside the mode). Exiting first would erase
                // the cursor before the terminate could read it, silently
                // reverting to the focused pane -- the exact behavior this
                // reorder exists to prevent.
                self.workspace.terminate_active_session();
                self.workspace.exit_workspace_mode();
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateAllDetachedSessions => {
                for summary in self.workspace.detached_session_summaries() {
                    self.workspace.terminate_session(summary.id);
                }
                self.reconcile(window, cx);
            }
            CommandId::OpenSessionManager => self.open_session_manager(window, cx),
            CommandId::SwitchModel => {
                if let Some(session) = self.active_agent_session() {
                    self.open_model_picker(session, window, cx);
                }
            }
            CommandId::OpenBoard => self.open_board_pane(window, cx),
            CommandId::OpenBoardRelatedItem
            | CommandId::BackBoardList
            | CommandId::AddBoardTask
            | CommandId::PostBoardMessage
            | CommandId::MoveBoardTaskUp
            | CommandId::MoveBoardTaskDown
            | CommandId::ReorderBoardTask
            | CommandId::SaveBoardState
            | CommandId::ToggleBoardClosed
            | CommandId::AddBoardDependency
            | CommandId::RemoveBoardDependency => {
                if let Some(view) = self.active_board_pane() {
                    view.update(cx, |view, cx| view.board_command(id, window, cx));
                }
            }
            CommandId::ReloadPreview => self.reload_active_preview(cx),
            CommandId::OpenBoardOrganizer => self.open_board_organizer(cx),
            CommandId::OpenBoardTaskSession => self.open_board_task_session(cx),
            CommandId::ToggleBoardExpansion => {
                if let Some(view) = self.active_board_pane() {
                    view.update(cx, |view, cx| view.toggle_expansion(cx));
                }
            }
            CommandId::ToggleBoardClosedVisibility => {
                if let Some(view) = self.active_board_pane() {
                    view.update(cx, |view, cx| view.toggle_closed_visibility(cx));
                }
            }
            // The font-size commands mutate the shell crate's live font
            // store (`terminal::font_size_store`) and refresh the window:
            // the next paint recomputes cell metrics from the new size,
            // resizes the PTY grid when cols/rows moved, and the shape
            // cache drops its rows via `CacheEpoch::font_size`. No-op
            // (no refresh) at a clamp bound or an already-configured
            // size, so holding the chord doesn't thrash repaints.
            CommandId::IncreaseFontSize => {
                if crate::terminal::adjust_font_size(crate::terminal::FONT_SIZE_STEP) {
                    window.refresh();
                }
            }
            CommandId::DecreaseFontSize => {
                if crate::terminal::adjust_font_size(-crate::terminal::FONT_SIZE_STEP) {
                    window.refresh();
                }
            }
            CommandId::ResetFontSize => {
                if crate::terminal::reset_font_size() {
                    window.refresh();
                }
            }
            CommandId::ApproveToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = session.read(cx).pending_approval_call_ids();
                    if let Some(call_id) = pending.first() {
                        session.read(cx).approve(call_id.clone());
                    }
                }
            }
            CommandId::DenyToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = session.read(cx).pending_approval_call_ids();
                    if let Some(call_id) = pending.first() {
                        session.read(cx).deny(call_id.clone(), None);
                    }
                }
            }
            CommandId::CancelAgentTurn => {
                if let Some(session) = self.active_agent_session() {
                    session.read(cx).cancel();
                }
            }
            CommandId::ContinueAgentTurn => {
                if let Some(session) = self.active_agent_session() {
                    session.read(cx).continue_turn();
                }
            }
            CommandId::ReloadConfig => match horizon_config::reload() {
                Ok(raw) => {
                    theme::live::apply_scheme(&raw, cx);
                    super::bindings::apply_bindings(cx, &raw);
                    window.refresh();
                    self.broadcast_terminal_color_scheme();
                    // `[provider]` is the daemon-owned half of the config:
                    // push it live without a `Reload Agent Runtime` (which
                    // is now scoped to agent-code reloads -- see
                    // `docs/terminald-split-design.md` decision 2). Fire-and-
                    // forget so the UI thread never blocks on the daemon.
                    if let Some(agentd) = self.agentd.as_ref() {
                        agentd.reload_provider_config();
                    }
                }
                Err(error) => eprintln!("reload-config failed: {error}"),
            },
            CommandId::OpenTerminalInSessionDirectory => {
                let workspace_root = self
                    .workspace
                    .active_session_id()
                    .and_then(|id| self.workspace.session_workspace_root(id))
                    .map(|path| path.to_path_buf());
                if let Some(workspace_root) = workspace_root {
                    self.open_terminal_in_directory(workspace_root, window, cx);
                }
            }
            // Agent runtime only, since the terminald split: terminal
            // sessions, their entities, and their pane views all stay
            // exactly where they are, so the shells running inside them
            // never notice (`docs/terminald-split-design.md` decision 2).
            CommandId::ReloadAgentRuntime => {
                if self.reload_in_progress {
                    return;
                }
                self.reload_in_progress = true;
                let old = self.agentd.take();
                if self.workspace_restore_failed {
                    self.workspace = Workspace::mvp();
                    self.restoring_workspace = false;
                    self.workspace_restore_failed = false;
                    self.persistence_ready = true;
                    self.persist_workspace();
                }
                self.pending_agent_spawns.clear();
                self.agent_sessions.clear();
                // The model picker's target session and its provider list
                // both die with the runtime: drop the modal instead of
                // leaving a surface whose confirm can only ever hit
                // "Unknown session" and whose list can never load.
                self.model_picker = None;
                self._model_picker_subscription = None;
                self.model_picker_target = None;
                // Only the agent panes' views are dropped: a terminal
                // pane's view holds live scrollback/selection state bound
                // to a session that is still running, and rebuilding it
                // would throw that away for no reason.
                self.panes.retain(|_, view| {
                    !matches!(view, PaneView::Composite(CompositePane::Agent(_)))
                });
                cx.notify();
                self.reload_agent_runtime(old, cx);
            }
            CommandId::ReloadTerminalRuntime => {
                if self.reload_in_progress {
                    return;
                }
                self.reload_in_progress = true;
                let old = self.terminald.take();
                self.terminald_slot.set(None);
                if self.workspace_restore_failed {
                    self.workspace = Workspace::mvp();
                    self.restoring_workspace = false;
                    self.workspace_restore_failed = false;
                    self.persistence_ready = true;
                    self.persist_workspace();
                } else {
                    prepare_workspace_for_terminal_runtime_reload(&mut self.workspace);
                    self.persist_workspace();
                }
                self.pending_terminal_spawns.clear();
                self.sessions.clear();
                // Only the terminal panes' views go: their sessions are
                // genuinely gone. The theme settings view stays even though
                // it holds a terminald handle -- it holds the *slot*, which
                // this command has just set to `None` and the respawn will
                // refill, so it never goes stale (see `TerminaldSlot`), and
                // rebuilding it would discard the user's unsaved seed edits.
                self.panes.retain(|_, view| {
                    !matches!(view, PaneView::Cached(CachedPaneLeaf::Terminal(_)))
                });
                self.last_focused_terminal = None;
                cx.notify();
                self.reload_terminal_runtime(old, cx);
            }
        }
    }

    /// `execute` for control-plane callers — public without exposing the
    /// whole command surface.
    pub(crate) fn execute_control_plane(
        &mut self,
        id: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute(id, window, cx);
    }

    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        // Close before exiting workspace mode, mirroring the
        // `TerminateActiveSession` arm: the close target is the pane the
        // cursor sits on (`Workspace::close_cursor_pane_or_tab`, resolving
        // through `cursor_pane_id` per the design's "commands act on the
        // cursor" rule), and `exit_workspace_mode` clears the cursor
        // first, which would silently revert the target to whichever pane
        // still holds keyboard focus.
        //
        // A last-pane tab falls through to closing the tab itself (the
        // model operation's job), so the only inert `x` left is a missing
        // cursor (zero tabs) -- and that alone must leave the mode and its
        // cursor untouched. Exiting the mode on an inert `x` read as
        // "x did nothing AND the mode turned off".
        if matches!(
            self.workspace.close_cursor_pane_or_tab(),
            CloseCursorOutcome::Noop
        ) {
            return;
        }
        // Either the pane's session or the closed tab's sessions were
        // detached by the model; `reconcile` keeps detached sessions alive
        // and drops only their pane views (see
        // `session_lifecycle::reconcile`). Same postlude as
        // `CommandId::CloseActiveTab`.
        self.workspace.exit_workspace_mode();
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// The tab strip's close button: close the tab at `index` -- the tab
    /// the user clicked, not the cursor's. Same postlude as the
    /// `CommandId::CloseActiveTab` arm (which stays the keyboard path):
    /// `close_tab_index` detaches the tab's sessions rather than
    /// terminating them (`docs/ux-principles.md`'s "Close, Detach, And
    /// Terminate" -- closing a surface never ends what it showed), and
    /// `reconcile` both drops the closed tab's pane views and persists the
    /// model. `exit_workspace_mode` before the close mirrors that arm: a
    /// mode cursor left pointing into a removed tab has no meaning, and a
    /// background-tab close is rare enough that "mode turned off" reads as
    /// the same simplification `CloseActiveTab` already makes.
    pub(super) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        // The index is render-time state and the dispatch is synchronous,
        // but this guard keeps a stale index a silent no-op instead of
        // relying on `close_tab_index`'s own out-of-range early return --
        // an empty detached-session return would otherwise read as success
        // for a pane-less tab, and an in-range-but-stale index could close
        // the wrong tab entirely.
        if index >= self.workspace.tab_count() {
            return;
        }
        self.workspace.exit_workspace_mode();
        self.workspace.close_tab_index(index);
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    pub(crate) fn session_summaries(&self) -> Vec<horizon_workspace::types::SessionSummary> {
        self.workspace.session_summaries()
    }

    /// Control-plane operations — the CLI's verbs, mirroring the Floem
    /// shell's `external_commands` semantics. The Rust family was renamed
    /// from `external_*` to `control_plane_*` so the prefix names the
    /// caller (the CLI's stable verb surface) instead of reading as
    /// "attach an external session"; the published string names are
    /// untouched. `activate: false` never steals focus.
    ///
    /// The one CLI verb with a fallback: an id the model has never seen may
    /// still exist inside `horizon-agentd` (a board-organizer/keeper
    /// session the daemon spawned on its own, invisible to the shell's
    /// pull-only registry until the next resume sweep). Instead of failing
    /// with "unknown session", the id is resolved against the daemon's
    /// inventory off-thread and adopted through the common adoption path
    /// when found. The reply is therefore optimistic: an id that isn't in
    /// the daemon's inventory either returns `Ok` and silently does
    /// nothing — acceptable for the human at the desktop this verb serves,
    /// wrong as a scripting check. The Manage Sessions modal and the board
    /// session openers use the strict [`Self::attach_known_session`]
    /// instead, so their error surfaces keep reporting the miss.
    pub(crate) fn control_plane_attach_session(
        &mut self,
        session_id: SessionId,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        if self.workspace.session_pane_kind(session_id).is_none() {
            self.spawn_agent_lookup_attach(session_id, activate, cx);
            return Ok(());
        }
        self.attach_known_session(session_id, activate, window, cx)
    }

    /// The strict attach: the model must already know the id. Shared by the
    /// CLI verb (once its lookup fallback has made the id known), the
    /// Manage Sessions modal, and the board session openers, whose error
    /// surfaces must keep reporting an actual miss.
    fn attach_known_session(
        &mut self,
        session_id: SessionId,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        self.workspace
            .attach_existing_session_to_split_activated(session_id, activate)
            .ok_or_else(|| "unknown session".to_string())?;
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    /// The lookup half of `control_plane_attach_session`'s fallback: one
    /// `session_list` pull on the background executor (the same async shape
    /// as `refresh_board_sessions`, with the same `same_runtime`/restore
    /// guards), then adoption through the common path and the normal
    /// attach. Reports nothing on failure — the CLI reply already went out,
    /// and an id the daemon doesn't know either is simply not attached.
    /// `session_list` is a blocking pull (up to `SYNC_REPLY_TIMEOUT`),
    /// which is exactly why this must never run on the UI thread.
    fn spawn_agent_lookup_attach(
        &self,
        session_id: SessionId,
        activate: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.agentd.clone() else {
            return;
        };
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let list_handle = handle.clone();
            let summary = cx
                .background_executor()
                .spawn(async move {
                    match list_handle.session_list() {
                        Ok(items) => daemon_summary_for(items, session_id),
                        Err(error) => {
                            eprintln!("attach lookup: failed to list agent sessions: {error}");
                            None
                        }
                    }
                })
                .await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    if shell.restoring_workspace
                        || shell
                            .agentd
                            .as_ref()
                            .is_none_or(|current| !current.same_runtime(&handle))
                    {
                        return;
                    }
                    let Some(summary) = summary else {
                        return;
                    };
                    if shell
                        .adopt_daemon_agent_session(
                            DaemonAgentAdoption::from(&summary),
                            ExistingAgentEntity::Reattach,
                            || handle.attach_session(summary.session_id),
                            cx,
                        )
                        .is_err()
                    {
                        return;
                    }
                    let _ = shell.attach_known_session(session_id, activate, window, cx);
                });
            });
        })
        .detach();
    }

    pub(crate) fn control_plane_terminate(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        if !self.workspace.terminate_session(session_id) {
            return Err("unknown session".to_string());
        }
        self.reconcile(window, cx);
        Ok(())
    }

    /// Session-targeted approve/deny/cancel/continue, for a control-plane
    /// caller that names an explicit `session_id` rather than "whichever
    /// pane is active" (unlike `CommandId::ApproveToolCall`/`DenyToolCall`/
    /// `CancelAgentTurn`/`ContinueAgentTurn`, which resolve against
    /// `active_agent_session`).
    pub(crate) fn control_plane_approve(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).approve(call_id);
        Ok(())
    }

    pub(crate) fn control_plane_deny(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).deny(call_id, reason);
        Ok(())
    }

    pub(crate) fn control_plane_cancel(
        &mut self,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).cancel();
        Ok(())
    }

    pub(crate) fn control_plane_continue_turn(
        &mut self,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).continue_turn();
        Ok(())
    }

    /// Delivers a user message to an already-running agent session -- the
    /// same `AgentSession::send_user_message` path the composer uses
    /// (`Command::UserMessage`), so a `WaitingForUser` session resumes with
    /// the text and a mid-turn session queues it as the next user turn,
    /// without any additional semantics to implement. v1 is attached-only:
    /// a detached session is not in `agent_sessions` and surfaces as
    /// "unknown session" (see issue 011's Notes).
    pub(crate) fn control_plane_send(
        &mut self,
        session_id: SessionId,
        text: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).send_user_message(text);
        Ok(())
    }

    pub(crate) fn control_plane_terminate_all_detached(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        for summary in self.workspace.detached_session_summaries() {
            self.workspace.terminate_session(summary.id);
        }
        self.reconcile(window, cx);
    }

    pub(crate) fn command_state_with(&self, cx: &App) -> CommandState {
        let (has_pending_approval, has_turn_in_flight, has_paused_turn) = self
            .active_agent_session()
            .map(|session| {
                let session = session.read(cx);
                let pending = !session.pending_approval_call_ids().is_empty();
                let in_flight = session.turn_in_flight();
                let paused = session.turn_halted();
                (pending, in_flight, paused)
            })
            .unwrap_or((false, false, false));
        CommandState {
            tab_count: self.workspace.tab_count(),
            visible_pane_count: self.workspace.visible_panes().len(),
            has_active_session: self.workspace.cursor_session_id().is_some(),
            has_cursor_board: self.active_board_pane().is_some(),
            detached_session_count: self.workspace.detached_session_count(),
            has_pending_approval,
            has_turn_in_flight,
            has_paused_turn,
            has_active_session_workspace_root: self
                .workspace
                .active_session_id()
                .and_then(|id| self.workspace.session_workspace_root(id))
                .is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use horizon_workspace::commands::CommandId;
    use horizon_workspace::{PaneKind, SessionKind, Workspace};

    use super::{command_blocked_by_restore, prepare_workspace_for_terminal_runtime_reload};
    #[test]
    fn stale_board_lookup_cannot_restore_a_just_terminated_session() {
        assert!(!super::board_session_still_requested(true, false));
        assert!(super::board_session_still_requested(true, true));
        // A later owner post starts a fresh lookup for the now-absent ID.
        assert!(super::board_session_still_requested(false, false));
    }

    #[test]
    fn board_inventory_retries_a_binding_before_daemon_installation() {
        let summary = horizon_agent::wire::SessionSummary {
            session_id: horizon_agent::contract::SessionId::new(),
            provider_id: horizon_agent::contract::ProviderId("mock".into()),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        };
        let requested = std::collections::HashSet::from([horizon_workspace::SessionId::from_uuid(
            summary.session_id.as_uuid(),
        )]);
        let mut calls = 0;
        let result = super::load_board_summaries(
            || {
                calls += 1;
                Ok(if calls == 1 {
                    vec![]
                } else {
                    vec![summary.clone()]
                })
            },
            &requested,
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result, vec![summary]);
    }

    // The board-adoption registration test moved to
    // `session_lifecycle::tests` with its implementation
    // (`register_daemon_agent_summary`), which is where the model-level
    // half of every daemon-agent adoption now lives.

    // `ensure_workspace_has_pane` lives in `super::super` (`workspace::
    // mod`), not here -- unlike `command_blocked_by_restore`/
    // `prepare_workspace_for_terminal_runtime_reload`, both defined in this
    // file, it's no longer called by any production code in `commands.rs`
    // (the 2026-07-18 "empty workspace is valid" change removed its
    // `TerminateActiveSession`/`control_plane_terminate` call sites); its one
    // remaining caller is `reload_terminal_runtime` in
    // `session_lifecycle`.
    use super::super::ensure_workspace_has_pane;

    #[test]
    fn terminal_reload_prep_removes_terminals_but_retains_agent_model_and_pane() {
        let mut workspace = Workspace::mvp();
        let agent_id = workspace.open_tab_with_new_session_activated(PaneKind::Agent, true);
        assert!(workspace.pane_location_for_session(agent_id).is_some());

        prepare_workspace_for_terminal_runtime_reload(&mut workspace);

        let summaries = workspace.session_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, agent_id);
        assert_eq!(summaries[0].kind, SessionKind::Agent);
        assert!(workspace.pane_location_for_session(agent_id).is_some());
    }

    #[test]
    fn terminal_runtime_reload_reseeds_a_terminal_when_no_pane_survives() {
        let mut workspace = Workspace::mvp();
        prepare_workspace_for_terminal_runtime_reload(&mut workspace);
        assert_eq!(workspace.tab_count(), 0);

        let session_id = ensure_workspace_has_pane(&mut workspace).expect("fresh terminal");

        assert_eq!(workspace.active_session_id(), Some(session_id));
        assert_eq!(
            workspace.session_pane_kind(session_id),
            Some(PaneKind::Terminal)
        );
    }

    #[test]
    fn failed_restore_allows_only_the_explicit_runtime_reload_commands() {
        // Both reloads are escape hatches out of a failed restore, and both
        // stay blocked while a restore is merely *in progress*.
        assert!(command_blocked_by_restore(
            true,
            false,
            CommandId::ReloadAgentRuntime
        ));
        assert!(command_blocked_by_restore(
            true,
            false,
            CommandId::ReloadTerminalRuntime
        ));
        assert!(command_blocked_by_restore(true, true, CommandId::NewTab));
        assert!(!command_blocked_by_restore(
            true,
            true,
            CommandId::ReloadAgentRuntime
        ));
        assert!(!command_blocked_by_restore(
            true,
            true,
            CommandId::ReloadTerminalRuntime
        ));
        assert!(!command_blocked_by_restore(false, false, CommandId::NewTab));
    }
}
