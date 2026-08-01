//! The command-model dispatch point (`execute`/`execute_external`) plus
//! the session-targeted `external_*` family the CLI control plane drives
//! (everything but `external_new_session`, which pairs with
//! `create_session` in `session_lifecycle` instead -- see that module's
//! doc comment).

use gpui::*;
use horizon_workspace::commands::{CommandId, CommandState};
use horizon_workspace::types::SessionKind;
use horizon_workspace::{SessionId, Workspace};

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

impl WorkspaceShell {
    /// The active pane's agent session, when it is an agent pane.
    fn active_agent_session(&self) -> Option<Entity<AgentSession>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let session_id = self.workspace.agent_session_id(pane_id)?;
        self.agent_sessions.get(&session_id).cloned()
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
                        session.read(cx).deny(call_id.clone());
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
                    theme::reload_from(&raw);
                    theme::apply_gpui_component_theme(cx);
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
    pub(crate) fn execute_external(
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
        self.workspace.exit_workspace_mode();
        // The model detaches the session; in M2 the view (and its PTY)
        // simply drops with it — close-vs-terminate parity needs the M3
        // Registry.
        self.workspace.close_active_pane();
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    pub(crate) fn session_summaries(&self) -> Vec<horizon_workspace::types::SessionSummary> {
        self.workspace.session_summaries()
    }

    /// External (control-plane) operations — the CLI's verbs, mirroring
    /// the Floem shell's `external_commands` semantics: `activate:
    /// false` never steals focus.
    pub(crate) fn external_attach(
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

    pub(crate) fn external_terminate(
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
    pub(crate) fn external_approve(
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

    pub(crate) fn external_deny(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).deny(call_id);
        Ok(())
    }

    pub(crate) fn external_cancel(
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

    pub(crate) fn external_continue_turn(
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
    pub(crate) fn external_send(
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

    pub(crate) fn external_terminate_all_detached(
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
    // `ensure_workspace_has_pane` lives in `super::super` (`workspace::
    // mod`), not here -- unlike `command_blocked_by_restore`/
    // `prepare_workspace_for_terminal_runtime_reload`, both defined in this
    // file, it's no longer called by any production code in `commands.rs`
    // (the 2026-07-18 "empty workspace is valid" change removed its
    // `TerminateActiveSession`/`external_terminate` call sites); its one
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
