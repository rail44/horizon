//! Independent daemon replacement sequences after command-layer teardown.

use super::{ensure_workspace_has_pane, WorkspaceShell};
use crate::runtime::{wait_for_drain, AgentdHandle, TerminaldHandle};
use gpui::*;

impl WorkspaceShell {
    /// Drains the explicit old *agent* runtime on a background thread, then
    /// creates exactly one fresh eager runtime and lists/loads persisted
    /// agents. The caller has already dropped every stale agent entity and
    /// agent pane view without sending semantic agent shutdown commands.
    ///
    /// Since the terminald split this touches nothing terminal-shaped: the
    /// terminal daemon, its PTYs, this shell's `TerminalSession` entities and
    /// their pane views all stay live across the whole sequence, so no
    /// terminal resume sweep is needed either (there is nothing to
    /// re-adopt — the attachments were never severed).
    pub(super) fn reload_agent_runtime(&self, old: Option<AgentdHandle>, cx: &mut Context<Self>) {
        let socket_path = horizon_wire::socket::default_agentd_socket_path();
        let restart_socket = socket_path.clone();
        let control_socket = self.socket_path.clone();
        let (drained_tx, mut drained_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            if let Some(handle) = old {
                if handle.begin_reload() {
                    if let Err(error) = wait_for_drain(&socket_path) {
                        eprintln!("horizon-agentd did not drain cleanly: {error}");
                    }
                }
                handle.stop_and_wait();
            }
            let _ = drained_tx.unbounded_send(());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            if drained_rx.next().await.is_none() {
                return;
            }
            let _ = this.update(cx, |shell, cx| {
                let (handle, host_tool_rx, workspace_root_rx) =
                    AgentdHandle::start(&restart_socket, &control_socket);
                shell.agentd = Some(handle.clone());
                shell.reload_in_progress = false;
                shell.wire_host_tools(handle.responder(), host_tool_rx, cx);
                shell.wire_workspace_root_updates(workspace_root_rx, cx);
                shell.spawn_agent_resume(handle, cx);
            });
        })
        .detach();
    }

    /// [`Self::reload_agent_runtime`]'s terminal-daemon counterpart
    /// (`docs/terminald-split-design.md` decision 3): drains the old
    /// `horizon-terminald`, which kills every PTY it hosts, then starts a
    /// fresh one and reseeds a pane so the workspace is not left empty by an
    /// operational restart the user did not ask to empty it.
    ///
    /// The caller has already terminated the terminal *model* sessions
    /// (`prepare_workspace_for_terminal_runtime_reload`) and dropped their
    /// entities and views, so what follows is a clean bring-up. The resume
    /// sweep still runs: if the old daemon refused to drain and the fresh
    /// connection lands back on it, its surviving sessions are re-adopted
    /// rather than orphaned.
    pub(super) fn reload_terminal_runtime(
        &self,
        old: Option<TerminaldHandle>,
        cx: &mut Context<Self>,
    ) {
        let socket_path = horizon_wire::socket::default_terminald_socket_path();
        let restart_socket = socket_path.clone();
        let control_socket = self.socket_path.clone();
        let window_handle = self.window;
        let (drained_tx, mut drained_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            if let Some(handle) = old {
                if handle.begin_reload() {
                    if let Err(error) = wait_for_drain(&socket_path) {
                        eprintln!("horizon-terminald did not drain cleanly: {error}");
                    }
                }
                handle.stop_and_wait();
            }
            let _ = drained_tx.unbounded_send(());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            if drained_rx.next().await.is_none() {
                return;
            }
            // Reconcile here rather than leaving it to the resume sweep:
            // that sweep returns early when the fresh daemon reports no
            // sessions (the normal case after a drain), which would leave
            // the reseeded model pane without an entity or a view.
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    ensure_workspace_has_pane(&mut shell.workspace);
                    let handle = TerminaldHandle::start(&restart_socket, &control_socket);
                    shell.terminald = Some(handle.clone());
                    shell.terminald_slot.set(Some(handle.clone()));
                    shell.reload_in_progress = false;
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                    shell.spawn_terminal_resume(handle, cx);
                });
            });
        })
        .detach();
    }
}
