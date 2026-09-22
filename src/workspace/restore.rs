//! Select restore candidates from both daemon inventories and persisted kinds.

use horizon_agent::wire::SessionSummary;
use horizon_terminal_core::TerminalSummary;
use horizon_workspace::{types::SessionKind, Workspace};
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

#[cfg(test)]
mod tests {
    use super::{RestoreCandidates, SessionSummary, TerminalSummary, Workspace};
    use horizon_agent::contract::{ProviderId, SessionId as AgentSessionId};
    use horizon_workspace::{PaneKind, SessionId};
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
        workspace.register_detached_session(PaneKind::Terminal, saved_terminal);
        workspace.register_detached_session(PaneKind::Agent, saved_agent);
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
        workspace.register_detached_session(PaneKind::Agent, id);
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
