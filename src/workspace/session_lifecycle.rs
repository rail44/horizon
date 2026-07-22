//! Session-creation and sessiond-runtime lifecycle: the one interactive
//! and control-plane session-creation paths (`create_session`/
//! `external_new_session`), pending-spawn staging consumed by
//! `reconcile`, the startup/reload resume sweeps
//! (`spawn_terminal_resume`/`spawn_agent_resume`/`spawn_workspace_restore`),
//! `reload_session_runtime`, and the terminal-exit-to-terminate wiring
//! (`wire_terminal_exit`/`handle_terminal_exited`). `reconcile` itself --
//! bringing the session store and pane views in line with the model --
//! lives here too, since every one of the above ends by calling it.

use std::collections::{HashMap, HashSet};

use gpui::*;
use horizon_terminal_core::{TerminalSize, TerminalSpawnSpec, DEFAULT_SCROLLBACK_LINES};
use horizon_workspace::types::SessionKind;
use horizon_workspace::{PaneKind, SessionId, SessionInventory, SplitAxis, ViewKind};
use uuid::Uuid;

use super::{ensure_workspace_has_pane, PaneView, WorkspaceShell};
use crate::agent::{AgentSession, AgentView};
use crate::markdown_viewer::MarkdownViewer;
use crate::sessiond::{wait_for_drain, SessiondHandle, SessiondResponder};
use crate::terminal::{TerminalSession, TerminalView};
use crate::theme;
use crate::theme_settings::ThemeSettingsView;
use crate::view_chooser::Placement;

type AgentSessionId = horizon_agent::contract::SessionId;

fn agent_session_id(id: SessionId) -> AgentSessionId {
    AgentSessionId::from_uuid(id.as_uuid())
}

#[derive(Clone)]
pub(super) struct PendingTerminalSpawn {
    source_session_id: Option<SessionId>,
    fallback_cwd: std::path::PathBuf,
}

/// Staged the same way as [`PendingTerminalSpawn`] (before a
/// session-creating workspace mutation, consumed by `reconcile`), but for
/// agent spawns' own two knobs (`docs/session-relationship-design.md`
/// decision 3): the pane this spawn derives from, and whether sessiond
/// should give it an isolated worktree. `Default` is "no source, not
/// isolated" -- `reconcile`'s fallback when nothing staged anything (e.g. a
/// resumed/attached session, never a fresh spawn).
#[derive(Clone, Default)]
pub(super) struct PendingAgentSpawn {
    source_session_id: Option<SessionId>,
    isolate: bool,
}

/// Kind-agnostic spawn-source resolution: an explicit target (e.g. the
/// `--split`/split-target session) wins, else whatever pane is currently
/// active. Shared by [`WorkspaceShell::pending_terminal_spawn`] and
/// [`WorkspaceShell::pending_agent_spawn`] -- terminal cwd inheritance and
/// agent lineage/isolation both need exactly the same "spawned from" pane.
fn resolve_spawn_source(
    explicit_source: Option<SessionId>,
    active_session: Option<SessionId>,
) -> Option<SessionId> {
    explicit_source.or(active_session)
}

fn terminal_fallback_cwd(
    current_dir: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    current_dir
        .or(home)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The default `workspace_root` for a brand-new agent session: the Horizon
/// process's own cwd, `None` only if that can't be read -- see
/// `wire::SessionNew::workspace_root`'s doc comment for why this, not the
/// spawn-source pane's cwd, is what's used today.
fn default_agent_workspace_root() -> Option<std::path::PathBuf> {
    std::env::current_dir().ok()
}

/// `docs/session-relationship-design.md` decision 4a's "Open Terminal in
/// Session Directory" spawn request: the cwd is pinned to `workspace_root`
/// directly, with no spawn-source pid inheritance (`source_session_id:
/// None`) -- unlike a plain new terminal, the target directory is already
/// known exactly, so there is nothing to inherit.
fn pinned_terminal_spawn(workspace_root: std::path::PathBuf) -> PendingTerminalSpawn {
    PendingTerminalSpawn {
        source_session_id: None,
        fallback_cwd: workspace_root,
    }
}

fn terminal_resume_candidates(
    summaries: Vec<horizon_terminal_core::TerminalSummary>,
    known: &std::collections::HashSet<SessionId>,
) -> Vec<Uuid> {
    let mut seen = std::collections::HashSet::new();
    summaries
        .into_iter()
        .filter_map(|summary| {
            let id = SessionId::from_uuid(summary.session_id);
            (!known.contains(&id) && seen.insert(id)).then_some(summary.session_id)
        })
        .collect()
}

impl WorkspaceShell {
    /// Bring the session store and the PaneId → view map in line with
    /// the model. Sessions the model no longer knows (terminated) are
    /// shut down and dropped; sessions without panes stay alive
    /// (detached); every pane gets a view bound to its session's entity,
    /// so a reattached pane resumes with scrollback intact.
    pub(super) fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summaries = self.workspace.session_summaries();
        let known: std::collections::HashSet<SessionId> =
            summaries.iter().map(|summary| summary.id).collect();
        self.sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        self.agent_sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        for summary in summaries {
            match summary.kind {
                SessionKind::Terminal => {
                    let id = summary.id;
                    if !self.sessions.contains_key(&id) {
                        let pending =
                            self.pending_terminal_spawns.remove(&id).unwrap_or_else(|| {
                                PendingTerminalSpawn {
                                    source_session_id: None,
                                    fallback_cwd: Self::default_terminal_cwd(),
                                }
                            });
                        let Some(sessiond) = self.sessiond.as_ref() else {
                            continue;
                        };
                        let wire = sessiond
                            .start_terminal(id.as_uuid(), self.terminal_spawn_spec(pending));
                        let exit_tx = self.terminal_exit_tx.clone();
                        self.sessions.insert(
                            id,
                            cx.new(|cx| TerminalSession::spawn(wire, id, exit_tx, cx)),
                        );
                    }
                }
                SessionKind::Agent => {
                    if self.agent_sessions.contains_key(&summary.id) {
                        continue;
                    }
                    let Some(handle) = self.sessiond.clone() else {
                        continue;
                    };
                    let provider_id =
                        horizon_agent::contract::ProviderRegistry::default().default_provider_id();
                    let role_id = self.pending_roles.remove(&summary.id);
                    let spawn = self
                        .pending_agent_spawns
                        .remove(&summary.id)
                        .unwrap_or_default();
                    // A brand-new agent session (this branch never runs for
                    // one `spawn_agent_resume`/`spawn_workspace_restore`
                    // already adopted -- see their own doc comments) has no
                    // `workspace_root` recorded yet; default it here and
                    // persist it on the model so `docs/session-relationship-
                    // design.md` decision 4a's "Open Terminal in Session
                    // Directory" command can read it back later, mirroring
                    // exactly what used to be computed inside
                    // `SessiondHandle::start_session` itself (see
                    // `wire::SessionNew::workspace_root`'s doc comment).
                    // For an isolated spawn this is only the *pre-isolation*
                    // value -- sessiond overrides it with the worktree path
                    // it creates and reports the authoritative root back via
                    // `wire::SessionSummary::workspace_root`, which the
                    // resume/restore sweeps below re-apply with
                    // `Workspace::set_session_workspace_root`.
                    let workspace_root =
                        summary.workspace_root.or_else(default_agent_workspace_root);
                    if let Some(root) = workspace_root.clone() {
                        self.workspace.set_session_workspace_root(summary.id, root);
                    }
                    let session_handle = handle.start_session(
                        agent_session_id(summary.id),
                        provider_id,
                        role_id,
                        workspace_root,
                        spawn.source_session_id.map(agent_session_id),
                        spawn.isolate,
                    );
                    self.agent_sessions.insert(
                        summary.id,
                        cx.new(|cx| AgentSession::new(session_handle, cx)),
                    );
                }
            }
        }

        let pane_ids = self.workspace.all_pane_ids();
        self.panes.retain(|id, _| pane_ids.contains(id));
        for pane_id in pane_ids {
            if self.panes.contains_key(&pane_id) {
                continue;
            }
            if let Some(session) = self
                .workspace
                .terminal_session_id(pane_id)
                .and_then(|id| self.sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Terminal(cx.new(|cx| TerminalView::new(session.clone(), window, cx))),
                );
            } else if let Some(session) = self
                .workspace
                .agent_session_id(pane_id)
                .and_then(|id| self.agent_sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Agent(cx.new(|cx| AgentView::new(session.clone(), window, cx))),
                );
            } else if matches!(
                self.workspace.pane_kind(pane_id),
                Some(PaneKind::View(ViewKind::ThemeSettings))
            ) {
                let sessiond = self.sessiond_slot.clone();
                self.panes.insert(
                    pane_id,
                    PaneView::ThemeSettings(
                        cx.new(|cx| ThemeSettingsView::new(sessiond, window, cx)),
                    ),
                );
            } else if matches!(
                self.workspace.pane_kind(pane_id),
                Some(PaneKind::View(ViewKind::Markdown))
            ) {
                if let Some(path) = self
                    .workspace
                    .pane_view_state(pane_id)
                    .and_then(|state| state.markdown_path())
                {
                    let path = path.clone();
                    self.panes.insert(
                        pane_id,
                        PaneView::Markdown(cx.new(|cx| MarkdownViewer::new(path, window, cx))),
                    );
                }
            }
        }
        self.persist_workspace();
        cx.notify();
    }

    /// Wires the host-tool responder for the already-adopted runtime:
    /// `workspace.snapshot` requests are answered on the UI thread from
    /// the live model, mirroring the Floem shell's
    /// `wire_host_tool_responder`.
    pub(super) fn wire_host_tools(
        &mut self,
        responder: SessiondResponder,
        host_tool_rx: crossbeam_channel::Receiver<horizon_agent::wire::HostToolRequest>,
        cx: &mut Context<Self>,
    ) {
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(request) = host_tool_rx.recv() {
                if async_tx.unbounded_send(request).is_err() {
                    return;
                }
            }
        });
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
    /// `sessiond::routing::Routes` -- see that `Control` variant's own doc
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
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(update) = workspace_root_rx.recv() {
                if async_tx.unbounded_send(update).is_err() {
                    return;
                }
            }
        });
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

    /// Lists the agent sessions hosted by the already-adopted runtime on a
    /// background thread, then adopts each as a detached
    /// session: registered in the model (so the session manager shows it)
    /// and attached over the wire (so its replayed transcript is ready
    /// when a pane picks it up). Shared by two callers: startup
    /// (`WorkspaceShell::new`, against a freshly opened window with no agent
    /// panes yet) and `Reload Session Runtime`
    /// (`Self::reload_session_runtime`, after the old connection has
    /// drained — see that function's doc comment for why its
    /// `agent_sessions`/agent-pane views are already cleared by the time
    /// this runs). Either way, the post-adopt `reconcile`/`focus_active`
    /// pass rebuilds any agent pane view whose session id this resume
    /// just reattached — a no-op at startup (no agent panes exist yet)
    /// and the reload's actual pane-rebuild step.
    pub(super) fn spawn_agent_resume(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (startup_tx, mut startup_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let summaries = list_handle.session_list();
            let _ = startup_tx.unbounded_send(summaries);
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let summaries = match startup_rx.next().await {
                Some(Ok(summaries)) => summaries,
                Some(Err(error)) => {
                    eprintln!("failed to list agent sessions: {error}");
                    Vec::new()
                }
                None => Vec::new(),
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    let Some(adopted) = shell.sessiond.clone() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }
                    for summary in summaries {
                        let session_id = SessionId::from_uuid(summary.session_id.as_uuid());
                        if shell.agent_sessions.contains_key(&session_id) {
                            continue;
                        }
                        if shell
                            .workspace
                            .session_pane_kind(session_id)
                            .is_some_and(|kind| kind != PaneKind::Agent)
                        {
                            eprintln!(
                                "ignoring agent session {}: its id is already used by a terminal",
                                session_id.as_uuid()
                            );
                            continue;
                        }
                        shell
                            .workspace
                            .register_detached_session(PaneKind::Agent, session_id);
                        // The daemon's own `SessionEntry` is authoritative
                        // for `workspace_root` -- for an isolated session
                        // this is the worktree path sessiond actually
                        // created, which nothing on the shell side could
                        // have known at spawn time (worktree creation
                        // finishes asynchronously, after `start_session`
                        // already returned -- see `wire::SessionSummary::
                        // workspace_root`'s doc comment). Overwrites
                        // whatever the model already had, if anything.
                        if let Some(root) = summary.workspace_root.clone() {
                            shell.workspace.set_session_workspace_root(session_id, root);
                        }
                        // Same authoritative-daemon-report treatment as
                        // `workspace_root` above, for the lineage edge
                        // (`docs/session-relationship-design.md` decisions
                        // 1-3): the shell never guesses this at spawn time
                        // (isolation may fail and degrade to a shared, edge-
                        // less spawn), so it's only ever populated here and
                        // in `spawn_workspace_restore`.
                        if let Some(parent) = summary.parent_session_id {
                            let parent_id = SessionId::from_uuid(parent.as_uuid());
                            shell.workspace.set_session_parent(session_id, parent_id);
                        }
                        let session_handle = adopted.attach_session(summary.session_id);
                        shell.agent_sessions.insert(
                            session_id,
                            cx.new(|cx| AgentSession::new(session_handle, cx)),
                        );
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Restores a persisted workspace only after both domain inventories are
    /// authoritative and every retained terminal has acknowledged Attach.
    /// Until this barrier opens, normal reconcile must not see the saved ids:
    /// it would interpret a missing entity as a request to create a new
    /// process with that id.
    pub(super) fn spawn_workspace_restore(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (list_tx, mut list_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let terminals = list_handle.terminal_list()?;
                let agents = list_handle.session_list()?;
                Ok::<_, String>((terminals, agents))
            })();
            let _ = list_tx.unbounded_send(result);
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let (terminal_summaries, agent_summaries) = match list_rx.next().await {
                Some(Ok(summaries)) => summaries,
                Some(Err(error)) => {
                    let _ = this.update(cx, |shell, cx| {
                        shell.fail_workspace_restore(error, cx);
                    });
                    return;
                }
                None => {
                    let _ = this.update(cx, |shell, cx| {
                        shell.fail_workspace_restore("inventory worker stopped", cx);
                    });
                    return;
                }
            };

            let candidates = this
                .update(cx, |shell, _| {
                    let adopted = shell.sessiond.as_ref()?;
                    if !adopted.same_runtime(&handle) {
                        return None;
                    }

                    let expected: HashMap<_, _> = shell
                        .workspace
                        .session_summaries()
                        .into_iter()
                        .map(|summary| (summary.id.as_uuid(), summary.kind))
                        .collect();
                    let terminal_ids: HashSet<_> = terminal_summaries
                        .into_iter()
                        .map(|summary| summary.session_id)
                        .collect();
                    // Captured before `agent_summaries` is consumed below --
                    // the daemon's own report of each session's
                    // `workspace_root` (the authoritative post-isolation
                    // worktree path for an isolated session; see
                    // `wire::SessionSummary::workspace_root`'s doc comment),
                    // applied to the surviving candidates further down.
                    let agent_workspace_roots: HashMap<Uuid, std::path::PathBuf> = agent_summaries
                        .iter()
                        .filter_map(|summary| {
                            summary
                                .workspace_root
                                .clone()
                                .map(|root| (summary.session_id.as_uuid(), root))
                        })
                        .collect();
                    // Same capture-before-consume treatment as `agent_
                    // workspace_roots` above, for the lineage edge
                    // (`docs/session-relationship-design.md` decisions
                    // 1-3): the daemon's report is authoritative, so this
                    // is applied to the surviving candidates further down
                    // exactly like the workspace root is.
                    let agent_parents: HashMap<Uuid, Uuid> = agent_summaries
                        .iter()
                        .filter_map(|summary| {
                            summary
                                .parent_session_id
                                .map(|parent| (summary.session_id.as_uuid(), parent.as_uuid()))
                        })
                        .collect();
                    let agent_ids: HashSet<_> = agent_summaries
                        .into_iter()
                        .map(|summary| summary.session_id.as_uuid())
                        .collect();
                    let conflicts: HashSet<_> =
                        terminal_ids.intersection(&agent_ids).copied().collect();
                    for id in &conflicts {
                        eprintln!(
                            "ignoring session {id}: it appears in both terminal and agent inventories"
                        );
                    }

                    let terminals = terminal_ids
                        .into_iter()
                        .filter(|id| !conflicts.contains(id))
                        .filter(|id| {
                            let matches = expected
                                .get(id)
                                .is_none_or(|kind| *kind == SessionKind::Terminal);
                            if !matches {
                                eprintln!(
                                    "ignoring terminal session {id}: persisted kind is agent"
                                );
                            }
                            matches
                        })
                        .collect::<Vec<_>>();
                    let agents = agent_ids
                        .into_iter()
                        .filter(|id| !conflicts.contains(id))
                        .filter(|id| {
                            let matches = expected
                                .get(id)
                                .is_none_or(|kind| *kind == SessionKind::Agent);
                            if !matches {
                                eprintln!(
                                    "ignoring agent session {id}: persisted kind is terminal"
                                );
                            }
                            matches
                        })
                        .collect::<Vec<_>>();
                    Some((terminals, agents, agent_workspace_roots, agent_parents))
                })
                .ok()
                .flatten();
            let Some((terminal_ids, agent_ids, agent_workspace_roots, agent_parents)) = candidates
            else {
                return;
            };

            let (attach_tx, mut attach_rx) = futures::channel::mpsc::unbounded();
            let attach_handle = handle.clone();
            std::thread::spawn(move || {
                let terminals = attach_handle.attach_terminals(terminal_ids);
                let agents = agent_ids
                    .into_iter()
                    .map(|id| {
                        let session_id = AgentSessionId::from_uuid(id);
                        (id, attach_handle.attach_session(session_id))
                    })
                    .collect::<Vec<_>>();
                let _ = attach_tx.unbounded_send((terminals, agents));
            });
            let Some((terminals, agents)) = attach_rx.next().await else {
                let _ = this.update(cx, |shell, cx| {
                    shell.fail_workspace_restore("attach worker stopped", cx);
                });
                return;
            };

            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, move |shell, cx| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }

                    let inventory = SessionInventory::new(
                        terminals
                            .iter()
                            .map(|(id, _)| SessionId::from_uuid(*id))
                            .collect(),
                        agents
                            .iter()
                            .map(|(id, _)| SessionId::from_uuid(*id))
                            .collect(),
                    );
                    if let Err(error) = shell.workspace.reconcile_session_inventory(&inventory) {
                        shell.fail_workspace_restore(
                            format_args!("inventory is invalid: {error}"),
                            cx,
                        );
                        return;
                    }

                    for (id, wire) in terminals {
                        let session_id = SessionId::from_uuid(id);
                        if shell.workspace.session_pane_kind(session_id)
                            == Some(PaneKind::Terminal)
                        {
                            let exit_tx = shell.terminal_exit_tx.clone();
                            shell.sessions.insert(
                                session_id,
                                cx.new(|cx| TerminalSession::spawn(wire, session_id, exit_tx, cx)),
                            );
                        }
                    }
                    for (id, wire) in agents {
                        let session_id = SessionId::from_uuid(id);
                        if shell.workspace.session_pane_kind(session_id) == Some(PaneKind::Agent) {
                            // See `spawn_agent_resume`'s matching comment:
                            // the daemon's report is authoritative,
                            // especially for an isolated session whose real
                            // (worktree) root was only known after
                            // `SessionNew` returned.
                            if let Some(root) = agent_workspace_roots.get(&id) {
                                shell
                                    .workspace
                                    .set_session_workspace_root(session_id, root.clone());
                            }
                            // See `spawn_agent_resume`'s matching comment
                            // for the lineage edge -- same authoritative
                            // treatment as `workspace_root` above.
                            if let Some(parent) = agent_parents.get(&id) {
                                shell
                                    .workspace
                                    .set_session_parent(session_id, SessionId::from_uuid(*parent));
                            }
                            shell.agent_sessions.insert(
                                session_id,
                                cx.new(|cx| AgentSession::new(wire, cx)),
                            );
                        }
                    }

                    // Adopted sessions carry the prior process's spawn-time
                    // scheme (or an even older one, resumed again); a
                    // theme change between runs would otherwise leave
                    // their OSC 10/11/12 replies stale until the next live
                    // theme apply. `adopted` is still valid here -- Attach
                    // already confirmed the daemon-side session exists, so
                    // this push lands after the session it targets is
                    // already routable, the same ordering guarantee
                    // `Create` gets by carrying the scheme inline.
                    adopted.broadcast_terminal_color_scheme(theme::terminal_color_scheme());

                    shell.restoring_workspace = false;
                    shell.workspace_restore_failed = false;
                    shell.persistence_ready = true;
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Discovers terminal sessions left alive by an earlier UI process and
    /// adopts them as detached sessions without delaying the fresh startup
    /// terminal. Listing and attaching are split by a UI-thread comparison:
    /// the just-created terminal (and any session created while List is in
    /// flight) must not have its existing route replaced by an Attach.
    pub(super) fn spawn_terminal_resume(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (list_tx, mut list_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let _ = list_tx.unbounded_send(list_handle.terminal_list());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let Some(Ok(summaries)) = list_rx.next().await else {
                return;
            };
            let candidates = this
                .update(cx, |shell, _| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return Vec::new();
                    };
                    if !adopted.same_runtime(&handle) {
                        return Vec::new();
                    }
                    let known = shell
                        .workspace
                        .session_summaries()
                        .into_iter()
                        .map(|summary| summary.id)
                        .collect();
                    terminal_resume_candidates(summaries, &known)
                })
                .unwrap_or_default();
            if candidates.is_empty() {
                return;
            }

            let (attach_tx, mut attach_rx) = futures::channel::mpsc::unbounded();
            let attach_handle = handle.clone();
            std::thread::spawn(move || {
                let attached = attach_handle.attach_terminals(candidates);
                let _ = attach_tx.unbounded_send(attached);
            });
            let Some(attached) = attach_rx.next().await else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }
                    for (wire_id, wire) in attached {
                        let session_id = SessionId::from_uuid(wire_id);
                        if shell
                            .workspace
                            .session_summaries()
                            .iter()
                            .any(|summary| summary.id == session_id)
                        {
                            continue;
                        }
                        shell
                            .workspace
                            .register_detached_session(PaneKind::Terminal, session_id);
                        let exit_tx = shell.terminal_exit_tx.clone();
                        shell.sessions.insert(
                            session_id,
                            cx.new(|cx| TerminalSession::spawn(wire, session_id, exit_tx, cx)),
                        );
                    }
                    // See `spawn_workspace_restore`'s matching comment: an
                    // adopted session carries a possibly-stale spawn-time
                    // scheme, and the Attach round-trip already confirmed
                    // it's routable before this push is sent.
                    adopted.broadcast_terminal_color_scheme(theme::terminal_color_scheme());
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Drains the explicit old runtime on a background thread, then creates
    /// exactly one fresh eager runtime and lists/loads persisted agents. The
    /// caller has already removed terminal model sessions and dropped every
    /// stale entity/view without sending semantic agent shutdown commands.
    pub(super) fn reload_session_runtime(
        &self,
        old: Option<SessiondHandle>,
        cx: &mut Context<Self>,
    ) {
        let socket_path = horizon_agent::socket::default_socket_path();
        let restart_socket = socket_path.clone();
        let control_socket = self.socket_path.clone();
        let (drained_tx, mut drained_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            if let Some(handle) = old {
                if handle.begin_reload() {
                    if let Err(error) = wait_for_drain(&socket_path) {
                        eprintln!("horizon-sessiond did not drain cleanly: {error}");
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
                ensure_workspace_has_pane(&mut shell.workspace);
                let (handle, host_tool_rx, workspace_root_rx) =
                    SessiondHandle::start(&restart_socket, &control_socket);
                shell.sessiond = Some(handle.clone());
                shell.sessiond_slot.set(Some(handle.clone()));
                shell.reload_in_progress = false;
                shell.wire_host_tools(handle.responder(), host_tool_rx, cx);
                shell.wire_workspace_root_updates(workspace_root_rx, cx);
                shell.spawn_agent_resume(handle, cx);
            });
        })
        .detach();
    }

    fn pending_terminal_spawn(&self, explicit_source: Option<SessionId>) -> PendingTerminalSpawn {
        PendingTerminalSpawn {
            source_session_id: resolve_spawn_source(
                explicit_source,
                self.workspace.active_session_id(),
            ),
            fallback_cwd: Self::default_terminal_cwd(),
        }
    }

    /// Stages an agent spawn's source pane and isolation choice for
    /// `reconcile` to consume -- `isolate` here is already the fully
    /// resolved per-spawn choice (origin default folded with any explicit
    /// override; see `create_session`/`external_new_session`), not a
    /// further default to apply.
    fn pending_agent_spawn(
        &self,
        explicit_source: Option<SessionId>,
        isolate: bool,
    ) -> PendingAgentSpawn {
        PendingAgentSpawn {
            source_session_id: resolve_spawn_source(
                explicit_source,
                self.workspace.active_session_id(),
            ),
            isolate,
        }
    }

    fn default_terminal_cwd() -> std::path::PathBuf {
        terminal_fallback_cwd(
            std::env::current_dir().ok(),
            std::env::var_os("HOME").map(std::path::PathBuf::from),
        )
    }

    fn terminal_spawn_spec(&self, pending: PendingTerminalSpawn) -> TerminalSpawnSpec {
        // `[terminal] shell_args`/`term`/`scrollback_lines` were retired in
        // the 2026-07-18 config-narrowing wave (see AGENTS.md's
        // "Configuration" section): each is now fixed. `shell` keeps its
        // existing $SHELL-else-/bin/sh logic, minus the former file
        // override.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        TerminalSpawnSpec {
            shell,
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            color_scheme: theme::terminal_color_scheme(),
            control_socket: self.socket_path.clone(),
            fallback_cwd: pending.fallback_cwd,
            spawn_source_session_id: pending.source_session_id.map(SessionId::as_uuid),
            initial_size: TerminalSize::new(80, 24),
        }
    }

    /// The one interactive session-creation path: what the view chooser
    /// confirms with. Terminal cwd and agent role ride the same staging
    /// maps `reconcile` consumes. `isolate` is the view chooser's own
    /// per-choice override of decision 3's palette-origin default (shared);
    /// `false` for every choice except the dedicated "Agent (Isolated
    /// Worktree)…" one (`ViewChoice::isolate`) -- ignored for a
    /// session-less `PaneKind::View` choice, same as `role_id`.
    pub(super) fn create_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        isolate: bool,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        if let PaneKind::View(view_kind) = kind {
            // A session-less first-party view: no session id to create,
            // no sessiond spawn, and no `pending_terminal_spawns`/
            // `pending_roles` bookkeeping -- those exist only for the
            // session-backed kinds handled below. `Markdown` is not created
            // through this path; it uses `open_markdown_file` instead.
            match placement {
                Placement::NewTab => {
                    self.workspace.open_view_tab(view_kind, None);
                }
                Placement::SplitRight | Placement::SplitDown => {
                    let axis = if placement == Placement::SplitRight {
                        SplitAxis::Horizontal
                    } else {
                        SplitAxis::Vertical
                    };
                    self.workspace
                        .split_active_tab_with_view(view_kind, None, axis);
                }
            }
            self.reconcile(window, cx);
            self.focus_active(window, cx);
            return;
        }
        let terminal_spawn =
            matches!(kind, PaneKind::Terminal).then(|| self.pending_terminal_spawn(None));
        // Palette origin defaults to shared, not isolated (`docs/session-
        // relationship-design.md` decision 3); `isolate` is the caller's
        // explicit opt-in (the view chooser's dedicated "Agent (Isolated
        // Worktree)…" choice), not a further default to apply here.
        let agent_spawn =
            matches!(kind, PaneKind::Agent).then(|| self.pending_agent_spawn(None, isolate));
        let session_id = match placement {
            Placement::NewTab => Some(
                self.workspace
                    .open_tab_with_new_session_activated(kind, true),
            ),
            Placement::SplitRight | Placement::SplitDown => {
                let axis = if placement == Placement::SplitRight {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                };
                self.workspace.active_session_id().and_then(|target| {
                    self.workspace
                        .split_session_with_new_session(target, kind, axis, true)
                })
            }
        };
        if let Some(session_id) = session_id {
            if let Some(spawn) = terminal_spawn {
                self.pending_terminal_spawns.insert(session_id, spawn);
            }
            if let Some(spawn) = agent_spawn {
                self.pending_agent_spawns.insert(session_id, spawn);
            }
            if let Some(role_id) = role_id {
                self.pending_roles.insert(session_id, role_id);
            }
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// `CommandId::OpenTerminalInSessionDirectory`
    /// (`docs/session-relationship-design.md` decision 4a): opens a new
    /// terminal tab pinned to `workspace_root`'s cwd -- v1's placement is a
    /// new tab, matching `create_session`'s own `Placement::NewTab` default
    /// for a plain new terminal. `execute` (`workspace/commands.rs`) is the
    /// only caller, having already resolved the active session's
    /// `workspace_root`; this stays a plain `PathBuf` parameter (not a
    /// `SessionId` re-lookup) since v1 targets only the active session --
    /// per-row "open its directory" on an arbitrary session is a later
    /// slice (see the design doc's decision 4b).
    pub(super) fn open_markdown_file(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        self.workspace.open_view_tab(
            ViewKind::Markdown,
            Some(horizon_workspace::ViewState::Markdown { path }),
        );
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    pub(super) fn open_terminal_in_directory(
        &mut self,
        workspace_root: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        let session_id = self
            .workspace
            .open_tab_with_new_session_activated(PaneKind::Terminal, true);
        self.pending_terminal_spawns
            .insert(session_id, pinned_terminal_spawn(workspace_root));
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// External (control-plane) operations — the CLI's verbs, mirroring
    /// the Floem shell's `external_commands` semantics: `activate:
    /// false` never steals focus. `prompt` (agent sessions only) sends
    /// the first user message right after the session starts — the
    /// create-with-prompt composite from the CLI design. `role_id` is
    /// fixed by the caller (e.g. `new-config-agent`), never client-supplied
    /// — see `pending_roles`. `isolate` is agent sessions' own per-spawn
    /// override of `docs/session-relationship-design.md` decision 3's
    /// origin default (CLI/control-plane origin defaults to isolated,
    /// mirroring `activate`'s opposite default): `None` applies that
    /// default, `Some` is an explicit override (the CLI's `--share`) --
    /// `control_plane::dispatch_invoke` already rejects a non-`None` value
    /// for a terminal spawn, so this never has to.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn external_new_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        prompt: Option<String>,
        isolate: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        let terminal_spawn = matches!(kind, PaneKind::Terminal)
            .then(|| self.pending_terminal_spawn(split.map(|(target, _)| target)));
        let agent_spawn = matches!(kind, PaneKind::Agent).then(|| {
            self.pending_agent_spawn(split.map(|(target, _)| target), isolate.unwrap_or(true))
        });
        let session_id = match split {
            Some((target, axis)) => self
                .workspace
                .split_session_with_new_session(target, kind, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_new_session_activated(kind, activate),
        };
        if let Some(spawn) = terminal_spawn {
            self.pending_terminal_spawns.insert(session_id, spawn);
        }
        if let Some(spawn) = agent_spawn {
            self.pending_agent_spawns.insert(session_id, spawn);
        }
        if let Some(role_id) = role_id {
            self.pending_roles.insert(session_id, role_id);
        }
        self.reconcile(window, cx);
        if let Some(prompt) = prompt {
            if let Some(session) = self.agent_sessions.get(&session_id) {
                session.read(cx).send_user_message(prompt);
            }
        }
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use horizon_terminal_core::TerminalSummary;
    use horizon_workspace::{PaneKind, SessionId, Workspace};

    use super::{
        pinned_terminal_spawn, resolve_spawn_source, terminal_fallback_cwd,
        terminal_resume_candidates,
    };

    #[test]
    fn explicit_split_target_wins_as_the_spawn_source() {
        let explicit = SessionId::new();
        let active = SessionId::new();
        assert_eq!(
            resolve_spawn_source(Some(explicit), Some(active)),
            Some(explicit)
        );
        assert_eq!(resolve_spawn_source(None, Some(active)), Some(active));
    }

    #[test]
    fn terminal_fallback_prefers_current_dir_then_home_then_dot() {
        let cwd = std::path::PathBuf::from("/workspace");
        let home = std::path::PathBuf::from("/home/test");
        assert_eq!(
            terminal_fallback_cwd(Some(cwd.clone()), Some(home.clone())),
            cwd
        );
        assert_eq!(terminal_fallback_cwd(None, Some(home.clone())), home);
        assert_eq!(
            terminal_fallback_cwd(None, None),
            std::path::PathBuf::from(".")
        );
    }

    #[test]
    fn open_terminal_in_directory_pins_the_target_cwd_with_no_source_inheritance() {
        // `docs/session-relationship-design.md` decision 4a: the spawn
        // request's cwd must be exactly the target session's
        // `workspace_root`, with no spawn-source pid inheritance (unlike a
        // plain new terminal, which sources from the active pane instead --
        // see `resolve_spawn_source`) since the directory is already known.
        let workspace_root = std::path::PathBuf::from("/some/agent/workspace");

        let spawn = pinned_terminal_spawn(workspace_root.clone());

        assert_eq!(spawn.fallback_cwd, workspace_root);
        assert_eq!(spawn.source_session_id, None);
    }

    #[test]
    fn terminal_resume_candidates_exclude_known_cross_kind_ids_and_duplicates() {
        let fresh_terminal = SessionId::new();
        let known_agent = SessionId::new();
        let first_survivor = SessionId::new();
        let second_survivor = SessionId::new();
        let known = [fresh_terminal, known_agent].into_iter().collect();
        let summaries = [
            fresh_terminal,
            first_survivor,
            known_agent,
            first_survivor,
            second_survivor,
        ]
        .into_iter()
        .map(|id| TerminalSummary {
            session_id: id.as_uuid(),
        })
        .collect();

        assert_eq!(
            terminal_resume_candidates(summaries, &known),
            vec![first_survivor.as_uuid(), second_survivor.as_uuid()]
        );
    }

    // `WorkspaceShell::handle_terminal_exited` (the receiving end of every
    // `TerminalSession`'s `exit_tx`) is itself GPUI-entity-shaped and not
    // unit-testable without a window, but its model-level step --
    // `Workspace::terminate_session`, leaving the workspace empty rather
    // than reseeding a terminal when that was its last session -- is the
    // same pure building block this module already tests elsewhere. The
    // tests below exercise exactly that, standing in for an end-to-end
    // exit-notification test.

    #[test]
    fn terminate_session_removes_it_whether_attached_or_detached() {
        // Decision 1: a PTY exit terminates its workspace session --
        // `handle_terminal_exited` calls `terminate_session` for whatever
        // session id the exit notification names, whether that session is
        // still attached to a pane or already sitting detached (a
        // session-manager entry that outlived its pane). Both must be
        // removed from the model.
        let mut workspace = Workspace::mvp();
        let attached = workspace.active_terminal_session_id().expect("session");
        let detached = SessionId::new();
        workspace.register_detached_session(PaneKind::Terminal, detached);
        assert!(!workspace.session_is_referenced(detached));

        assert!(workspace.terminate_session(attached));
        assert!(workspace.terminate_session(detached));

        assert!(workspace.session_summaries().is_empty());
    }

    #[test]
    fn terminating_the_last_session_leaves_an_empty_persistable_workspace_with_no_reseed() {
        // 2026-07-18 owner clarification, superseding `704657b`'s
        // auto-reseed guard: `WorkspaceState::validate` now accepts a
        // zero-tab workspace, so `handle_terminal_exited` (and every other
        // termination path) leaves it empty instead of auto-creating a
        // terminal the user didn't ask for.
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();

        assert_eq!(workspace.tab_count(), 0);
        assert!(workspace.session_summaries().is_empty());
        assert!(workspace.to_persisted_json().is_ok());
    }

    // `spawn_agent_resume`/`spawn_workspace_restore` are themselves
    // GPUI-entity/async-shaped and not unit-testable without a window and a
    // live sessiond connection, same as `handle_terminal_exited` above --
    // but their model-level step (`Workspace::set_session_workspace_root`,
    // called with `wire::SessionSummary.workspace_root` once the daemon's
    // own report of it arrives) is the same pure building block, standing
    // in for an end-to-end resume/adopt test.

    #[test]
    fn an_isolated_sessions_reported_workspace_root_wins_over_the_shells_pre_spawn_value() {
        // `docs/session-relationship-design.md`: `reconcile`'s Agent branch
        // records a session's *pre-isolation* `workspace_root` on the model
        // right away (the value it sent in `SessionNew`), since sessiond's
        // real isolated worktree only resolves asynchronously afterward.
        // `spawn_agent_resume`/`spawn_workspace_restore` must overwrite that
        // with whatever `wire::SessionSummary.workspace_root` the daemon
        // reports later -- the authoritative post-isolation root -- not
        // leave the stale pre-spawn value standing.
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        workspace.register_detached_session(PaneKind::Agent, session_id);

        let pre_spawn_root = std::path::PathBuf::from("/home/user/project");
        workspace.set_session_workspace_root(session_id, pre_spawn_root.clone());
        assert_eq!(
            workspace.session_workspace_root(session_id),
            Some(pre_spawn_root.as_path())
        );

        let isolated_root =
            std::path::PathBuf::from("/home/user/project/.horizon/worktrees/abcd1234");
        workspace.set_session_workspace_root(session_id, isolated_root.clone());

        assert_eq!(
            workspace.session_workspace_root(session_id),
            Some(isolated_root.as_path()),
            "the daemon-reported root must win over the shell's pre-spawn value"
        );
    }

    /// `wire_workspace_root_updates` is, like `spawn_agent_resume`/
    /// `spawn_workspace_restore` above, GPUI-entity/async-shaped and not
    /// unit-testable without a window and a live sessiond connection -- but
    /// its whole fold body is exactly these two model setters, called
    /// together the moment a `wire::Control::WorkspaceRootResolved`
    /// announcement arrives. Unlike the test above, this never goes through
    /// a resume/restore sweep at all: it stands in for a session created and
    /// used within one continuous run, which used to keep its pre-spawn
    /// guess (and no lineage edge) until the next sweep -- the live push
    /// this test's building block backs closes exactly that gap.
    #[test]
    fn a_freshly_isolated_sessions_live_announcement_updates_root_and_parent_together() {
        let mut workspace = Workspace::mvp();
        let session_id = SessionId::new();
        let parent_id = SessionId::new();
        workspace.register_detached_session(PaneKind::Agent, session_id);

        let pre_spawn_root = std::path::PathBuf::from("/home/user/project");
        workspace.set_session_workspace_root(session_id, pre_spawn_root);

        // What `wire_workspace_root_updates` does with an incoming
        // `WorkspaceRootResolved { workspace_root, parent_session_id }`.
        let isolated_root =
            std::path::PathBuf::from("/home/user/project/.horizon/worktrees/abcd1234");
        workspace.set_session_workspace_root(session_id, isolated_root.clone());
        workspace.set_session_parent(session_id, parent_id);

        assert_eq!(
            workspace.session_workspace_root(session_id),
            Some(isolated_root.as_path()),
            "the live announcement's root must be visible without any resume/restore sweep"
        );
        let summary = workspace
            .session_summaries()
            .into_iter()
            .find(|summary| summary.id == session_id)
            .expect("the session should still be registered");
        assert_eq!(
            summary.parent_session_id,
            Some(parent_id),
            "the live announcement's lineage edge must be visible without any sweep either"
        );
    }
}
