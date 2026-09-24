//! Apply session notifications to the model and route host requests and desktop feedback.

use super::WorkspaceShell;
use crate::runtime::AgentdResponder;
use gpui::*;
use horizon_workspace::SessionId;

impl WorkspaceShell {
    /// Wires the host-tool responder for the already-adopted runtime:
    /// `workspace.snapshot` requests are answered on the UI thread from
    /// the live model, mirroring the Floem shell's
    /// `wire_host_tool_responder`.
    pub(super) fn wire_host_tools(
        &mut self,
        responder: AgentdResponder,
        host_tool_rx: crossbeam_channel::Receiver<horizon_agent::wire::HostToolRequest>,
        cx: &mut Context<Self>,
    ) {
        let mut async_rx = crate::runtime::event_stream(host_tool_rx);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(request) = async_rx.next().await {
                let output = this
                    .update(cx, |shell, _| match request.tool_id.as_str() {
                        "workspace.snapshot" => {
                            horizon_workspace::snapshot::workspace_snapshot(&shell.workspace)
                        }
                        other => serde_json::json!({
                            "error": format!("unknown host tool `{other}`")
                        }),
                    })
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "the workspace shell is gone" }),
                    );
                responder.respond_host_tool(horizon_agent::wire::HostToolResponse {
                    request_id: request.request_id,
                    output: output.into(),
                });
            }
        })
        .detach();
    }

    /// Wires the live push of a freshly isolated session's authoritative
    /// `workspace_root`/`parent_session_id` (`wire::Control::
    /// WorkspaceRootResolved`, routed onto its own process-wide channel by
    /// `runtime::routing::AgentRoutes` -- see that `Control` variant's own doc
    /// comment) straight into the model the moment it arrives, closing the
    /// gap `spawn_agent_resume`/`spawn_workspace_restore` only closed on the
    /// next resume/reload sweep: a session created and used within one
    /// continuous run now sees its corrected root/parent immediately.
    /// Bridged to async the same way `wire_host_tools` bridges its own
    /// crossbeam receiver.
    pub(super) fn wire_workspace_root_updates(
        &mut self,
        workspace_root_rx: crossbeam_channel::Receiver<(
            horizon_agent::contract::SessionId,
            horizon_agent::wire::WorkspaceRootResolved,
        )>,
        cx: &mut Context<Self>,
    ) {
        let mut async_rx = crate::runtime::event_stream(workspace_root_rx);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some((session_id, resolved)) = async_rx.next().await {
                let session_id = SessionId::from_uuid(session_id.as_uuid());
                let _ = this.update(cx, |shell, cx| {
                    shell
                        .workspace
                        .set_session_workspace_root(session_id, resolved.workspace_root);
                    if let Some(parent) = resolved.parent_session_id {
                        shell
                            .workspace
                            .set_session_parent(session_id, SessionId::from_uuid(parent.as_uuid()));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Wires the receiving end of every `TerminalSession`'s `exit_tx`: a PTY
    /// child exiting (e.g. the user typing `exit`) notifies the shell with
    /// its session id, and the shell terminates that workspace session --
    /// "shell exit terminates the session" (decision 1). Already async
    /// (`TerminalSession::spawn` hands out a `futures` unbounded sender), so
    /// unlike `wire_host_tools` this needs no blocking-to-async bridge
    /// thread, just the pump.
    pub(super) fn wire_terminal_exit(
        &self,
        mut exit_rx: futures::channel::mpsc::UnboundedReceiver<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(session_id) = exit_rx.next().await {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |shell, cx| {
                        shell.handle_terminal_exited(session_id, window, cx);
                    });
                });
            }
        })
        .detach();
    }

    /// Wires the receiving end of every session's `title_tx`: a
    /// `TerminalSession` (OSC 0/2 title or its reset) or an `AgentSession`
    /// (first user message) reporting a content-derived title. The model
    /// decides whether the report changes anything
    /// (`set_session_derived_title` dedupes and honors manual titles), so
    /// the pump persists/repaints only on real changes. Already async
    /// (`futures` unbounded senders, like `wire_terminal_exit`), so no
    /// blocking-to-async bridge is needed.
    pub(super) fn wire_session_title_updates(
        &self,
        mut title_rx: futures::channel::mpsc::UnboundedReceiver<(SessionId, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some((session_id, title)) = title_rx.next().await {
                let _ = this.update(cx, |shell, cx| {
                    if shell
                        .workspace
                        .set_session_derived_title(session_id, title.as_deref())
                    {
                        shell.persist_workspace();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Wires the receiving end of every session's `notify_tx`: OSC 9/777
    /// desktop-notification requests (`TerminalUpdate::Notification`).
    /// Surfacing is decided per request against live focus state
    /// (`should_surface_notification`); a surfaced request is posted to
    /// the OS via gpui's unified system-notification API
    /// (`crate::desktop_notify::post`) and immediately forgotten — no
    /// per-post future is awaited, so one unnoticed banner can never
    /// stall later ones. A banner-body activation reaches
    /// `wire_notification_responses`' router, which answers with
    /// `reveal_session`. Same async shape as
    /// `wire_session_title_updates`: `futures` unbounded senders.
    pub(super) fn wire_terminal_notifications(
        &self,
        mut notify_rx: futures::channel::mpsc::UnboundedReceiver<(
            SessionId,
            horizon_terminal_core::TerminalNotification,
        )>,
        cx: &mut Context<Self>,
    ) {
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some((session_id, notification)) = notify_rx.next().await {
                let surface = window_handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |shell, _cx| {
                            shell.should_surface_notification(session_id, window)
                        })
                        .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if !surface {
                    continue;
                }
                // Fire-and-forget: gpui's platform backend owns delivery
                // and reports its failures through the `log` facade. A
                // banner-body activation arrives via
                // `wire_notification_responses` — `reveal_session` lives
                // there now — so nothing here waits on the user.
                crate::desktop_notify::post(session_id, notification.title, notification.body, cx);
            }
        })
        .detach();
    }

    /// Registers the one system-notification response router: a
    /// banner-body activation (`action_id: None` — Horizon posts no
    /// action buttons) with a tag matching a session id reveals that
    /// session's pane and foregrounds the window. This is the click
    /// answer for every posted notification, one router instead of one
    /// detached watcher per post; dismissals and expiries produce no
    /// response, so nothing is parked awaiting them. Responses arrive on
    /// the main thread (gpui pumps the platform backend's channel through
    /// the foreground executor), where `reveal_session` needs to run
    /// anyway.
    pub(super) fn wire_notification_responses(&self, cx: &mut Context<Self>) {
        let shell = cx.entity().downgrade();
        let window_handle = self.window;
        cx.on_system_notification_response(move |response, cx| {
            if response.action_id.is_some() {
                return;
            }
            let Some(session_id) = crate::desktop_notify::session_from_tag(&response.tag) else {
                return;
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.reveal_session(session_id, window, cx);
                });
            });
        });
    }

    /// Brings the pane hosting `session_id` to the front — the answer to a
    /// clicked notification banner: activate the pane's tab and split
    /// position (`Workspace::pane_location_for_session` +
    /// `activate_pane_index`, the pair the model documents for exactly
    /// this resolution), refocus, and foreground the window so the pane is
    /// actually on screen. A detached or terminated session has no pane to
    /// reveal — no layout change, no window steal.
    fn reveal_session(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        let Some((tab_index, pane_index)) = self.workspace.pane_location_for_session(session_id)
        else {
            return;
        };
        self.workspace.activate_pane_index(tab_index, pane_index);
        self.focus_active(window, cx);
        cx.notify();
        window.activate_window();
    }

    /// Terminates the workspace session whose shell just exited -- whether
    /// it was attached to a pane or sitting detached (session-manager
    /// entry), `terminate_session` handles both uniformly. If this emptied
    /// the workspace, it simply stays empty: an empty workspace is a
    /// valid, persistable state (2026-07-18 owner clarification), not
    /// something to paper over by auto-creating a terminal the user didn't
    /// ask for. Ignored while a restore is in progress: the session store
    /// isn't reconciled with the model yet, so there is nothing meaningful
    /// to terminate.
    fn handle_terminal_exited(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        if !self.workspace.terminate_session(session_id) {
            return;
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }
}
