//! Restore a workspace in inventory, attachment, and model-adoption phases.

use gpui::*;
use horizon_agent::wire::SessionSummary;
use horizon_terminal_core::TerminalSummary;
use horizon_workspace::{types::SessionKind, PaneKind, SessionId, SessionInventory, Workspace};

use super::WorkspaceShell;
use crate::agent::AgentSession;
use crate::runtime::{AgentSessionHandle, AgentdHandle, TerminalSessionHandle, TerminaldHandle};
use crate::theme;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;

pub(super) struct RestoreCandidates {
    pub(super) terminals: Vec<Uuid>,
    pub(super) agents: Vec<Uuid>,
    pub(super) agent_workspace_roots: HashMap<Uuid, PathBuf>,
    pub(super) agent_parents: HashMap<Uuid, Uuid>,
}

impl RestoreCandidates {
    /// Runtime generation checks stay with the async caller, before selection
    /// and again before attachment results are applied to the workspace.
    pub(super) fn select(
        workspace: &Workspace,
        terminal_summaries: Vec<TerminalSummary>,
        agent_summaries: Vec<SessionSummary>,
    ) -> Self {
        let expected: HashMap<_, _> = workspace
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
        let conflicts: HashSet<_> = terminal_ids.intersection(&agent_ids).copied().collect();
        for id in &conflicts {
            eprintln!("ignoring session {id}: it appears in both terminal and agent inventories");
        }

        let terminals = terminal_ids
            .into_iter()
            .filter(|id| !conflicts.contains(id))
            .filter(|id| {
                let matches = expected
                    .get(id)
                    .is_none_or(|kind| *kind == SessionKind::Terminal);
                if !matches {
                    eprintln!("ignoring terminal session {id}: persisted kind is agent");
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
                    eprintln!("ignoring agent session {id}: persisted kind is terminal");
                }
                matches
            })
            .collect::<Vec<_>>();
        Self {
            terminals,
            agents,
            agent_workspace_roots,
            agent_parents,
        }
    }
}

/// Both handles identify the generations the entire restore belongs to.
/// Reloading either runtime invalidates inventory and attachment results.
#[derive(Clone)]
struct RestoreRuntimes {
    agents: AgentdHandle,
    terminals: TerminaldHandle,
}

impl RestoreRuntimes {
    fn is_current(&self, shell: &WorkspaceShell) -> bool {
        shell
            .agentd
            .as_ref()
            .is_some_and(|current| current.same_runtime(&self.agents))
            && shell
                .terminald
                .as_ref()
                .is_some_and(|current| current.same_runtime(&self.terminals))
    }

    fn inventory(&self) -> Result<(Vec<TerminalSummary>, Vec<SessionSummary>), String> {
        // Both inventories must answer before missing sessions can be reconciled.
        let terminals = self.terminals.terminal_list()?;
        let agents = self.agents.session_list()?;
        Ok((terminals, agents))
    }

    fn attach(&self, candidates: RestoreCandidates) -> RestoredSessions {
        let terminals = self.terminals.attach_terminals(candidates.terminals);
        let agents = candidates
            .agents
            .into_iter()
            .map(|id| {
                let session_id = horizon_agent::contract::SessionId::from_uuid(id);
                (id, self.agents.attach_session(session_id))
            })
            .collect();
        RestoredSessions {
            terminals,
            agents,
            agent_workspace_roots: candidates.agent_workspace_roots,
            agent_parents: candidates.agent_parents,
        }
    }
}

/// Successful terminal attachments and routed agent handles, plus the daemon's
/// authoritative metadata captured before attachment. Agent attachment retains
/// its existing asynchronous completion/error contract.
struct RestoredSessions {
    terminals: Vec<(Uuid, TerminalSessionHandle)>,
    agents: Vec<(Uuid, AgentSessionHandle)>,
    agent_workspace_roots: HashMap<Uuid, PathBuf>,
    agent_parents: HashMap<Uuid, Uuid>,
}

impl WorkspaceShell {
    /// Release the restore barrier only after both inventories and attachment
    /// results belong to the current runtime generations.
    pub(super) fn spawn_workspace_restore(
        &self,
        handle: AgentdHandle,
        terminal_handle: TerminaldHandle,
        cx: &mut Context<Self>,
    ) {
        let runtimes = RestoreRuntimes {
            agents: handle,
            terminals: terminal_handle,
        };
        let window_handle = self.window;
        let (list_tx, mut list_rx) = futures::channel::mpsc::unbounded();
        let list_runtimes = runtimes.clone();
        std::thread::spawn(move || {
            let _ = list_tx.unbounded_send(list_runtimes.inventory());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let (terminals, agents) = match list_rx.next().await {
                Some(Ok(summaries)) => summaries,
                Some(Err(error)) => {
                    let _ = this.update(cx, |shell, cx| shell.fail_workspace_restore(error, cx));
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
                    if !runtimes.is_current(shell) {
                        return None;
                    }
                    Some(RestoreCandidates::select(
                        &shell.workspace,
                        terminals,
                        agents,
                    ))
                })
                .ok()
                .flatten();
            let Some(candidates) = candidates else {
                return;
            };

            let (attach_tx, mut attach_rx) = futures::channel::mpsc::unbounded();
            let attach_runtimes = runtimes.clone();
            std::thread::spawn(move || {
                let _ = attach_tx.unbounded_send(attach_runtimes.attach(candidates));
            });
            let Some(attached) = attach_rx.next().await else {
                let _ = this.update(cx, |shell, cx| {
                    shell.fail_workspace_restore("attach worker stopped", cx);
                });
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, move |shell, cx| {
                    shell.apply_workspace_restore(&runtimes, attached, window, cx);
                });
            });
        })
        .detach();
    }

    fn apply_workspace_restore(
        &mut self,
        runtimes: &RestoreRuntimes,
        restored: RestoredSessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !runtimes.is_current(self) {
            return;
        }
        let RestoredSessions {
            terminals,
            agents,
            agent_workspace_roots,
            agent_parents,
        } = restored;
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
        if let Err(error) = self.workspace.reconcile_session_inventory(&inventory) {
            self.fail_workspace_restore(format_args!("inventory is invalid: {error}"), cx);
            return;
        }

        for (id, wire) in terminals {
            let session_id = SessionId::from_uuid(id);
            if self.workspace.session_pane_kind(session_id) == Some(PaneKind::Terminal) {
                self.install_terminal_session(session_id, wire, cx);
            }
        }
        for (id, wire) in agents {
            let session_id = SessionId::from_uuid(id);
            if self.workspace.session_pane_kind(session_id) == Some(PaneKind::Agent) {
                // See `spawn_agent_resume`'s matching comment:
                // the daemon's report is authoritative,
                // especially for an isolated session whose real
                // (worktree) root was only known after
                // `SessionNew` returned.
                if let Some(root) = agent_workspace_roots.get(&id) {
                    self.workspace
                        .set_session_workspace_root(session_id, root.clone());
                }
                // See `spawn_agent_resume`'s matching comment
                // for the lineage edge -- same authoritative
                // treatment as `workspace_root` above.
                if let Some(parent) = agent_parents.get(&id) {
                    self.workspace
                        .set_session_parent(session_id, SessionId::from_uuid(*parent));
                }
                let title_tx = self.session_title_tx.clone();
                self.agent_sessions.insert(
                    session_id,
                    cx.new(|cx| AgentSession::new(wire, session_id, title_tx, cx)),
                );
            }
        }

        // Adopted sessions carry the prior process's spawn-time
        // scheme (or an even older one, resumed again); a
        // theme change between runs would otherwise leave
        // their OSC 10/11/12 replies stale until the next live
        // theme apply. The runtime pair is still current -- Attach
        // already confirmed the daemon-side session exists, so
        // this push lands after the session it targets is
        // already routable, the same ordering guarantee
        // `Create` gets by carrying the scheme inline.
        runtimes
            .terminals
            .broadcast_terminal_color_scheme(theme::terminal_color_scheme());

        self.restoring_workspace = false;
        self.workspace_restore_failed = false;
        self.persistence_ready = true;
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{RestoreCandidates, SessionSummary, TerminalSummary, Workspace};
    use horizon_agent::contract::{ProviderId, SessionId as AgentSessionId};
    use horizon_workspace::SessionId;
    use std::collections::HashSet;
    use uuid::Uuid;

    fn agent(id: Uuid) -> SessionSummary {
        SessionSummary {
            session_id: AgentSessionId::from_uuid(id),
            provider_id: ProviderId("test".into()),
            role_id: None,
            parent_session_id: None,
            workspace_root: None,
        }
    }

    #[test]
    fn conflicts_and_wrong_saved_kinds_are_excluded_but_unknown_sessions_survive() {
        let mut workspace = Workspace::mvp();
        let saved_terminal = SessionId::new();
        let saved_agent = SessionId::new();
        workspace
            .register_detached_session(horizon_workspace::SessionKind::Terminal, saved_terminal);
        workspace.register_detached_session(horizon_workspace::SessionKind::Agent, saved_agent);
        let conflict = Uuid::new_v4();
        let new_terminal = Uuid::new_v4();
        let new_agent = Uuid::new_v4();
        let selected = RestoreCandidates::select(
            &workspace,
            [conflict, saved_agent.as_uuid(), new_terminal, new_terminal]
                .map(|session_id| TerminalSummary { session_id })
                .to_vec(),
            [conflict, saved_terminal.as_uuid(), new_agent]
                .map(agent)
                .to_vec(),
        );
        assert_eq!(
            selected.terminals.into_iter().collect::<HashSet<_>>(),
            HashSet::from([new_terminal])
        );
        assert_eq!(
            selected.agents.into_iter().collect::<HashSet<_>>(),
            HashSet::from([new_agent])
        );
    }

    #[test]
    fn matching_agents_keep_daemon_workspace_and_lineage_metadata() {
        let mut workspace = Workspace::mvp();
        let id = SessionId::new();
        workspace.register_detached_session(horizon_workspace::SessionKind::Agent, id);
        let parent = Uuid::new_v4();
        let mut summary = agent(id.as_uuid());
        summary.workspace_root = Some("/restored-worktree".into());
        summary.parent_session_id = Some(AgentSessionId::from_uuid(parent));
        let selected = RestoreCandidates::select(&workspace, vec![], vec![summary]);
        assert_eq!(selected.agents, [id.as_uuid()]);
        assert_eq!(
            selected.agent_workspace_roots[&id.as_uuid()].to_str(),
            Some("/restored-worktree")
        );
        assert_eq!(selected.agent_parents[&id.as_uuid()], parent);
        let empty = RestoreCandidates::select(&workspace, vec![], vec![]);
        assert!(empty.terminals.is_empty() && empty.agents.is_empty());
        assert!(empty.agent_workspace_roots.is_empty() && empty.agent_parents.is_empty());
    }
}
