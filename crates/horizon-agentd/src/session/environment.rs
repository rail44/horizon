//! Build a session's tool environment and activate a replacement durably.

use super::board::board_host_for;
use super::events::send_session_event;
use super::exploration::AgentdExplorationHost;
use super::setup::{
    configured_domains, configured_filesystem_grants, configured_loopback_connect,
    configured_mach_services, project_is_trusted, skill_discovery_root, tool_session_builder_for,
};
use super::state::{lock_unpoisoned, AgentdState};
use crossbeam_channel::Sender;
use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{Command, Event, ProviderId, SessionId};
use horizon_agent::judge::JudgeHandle;
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::PersistedSessionContext;
use horizon_agent::roles::RoleId;
use horizon_agent::skills::SkillRegistry;
use horizon_agent::tools::{
    register_exploration_host, register_session_runtime, RecallContext, SessionDomainPolicy,
    ToolCompletion, ToolSessionState,
};
use horizon_agent::wire::AgentWireEvent;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Stable session identity and spawn-time configuration, shared by initial
/// construction and later activation. Config reloads affect future sessions.
pub(super) struct SessionEnvironment<'a> {
    pub(super) state: &'a Arc<AgentdState>,
    pub(super) session_id: SessionId,
    pub(super) provider_id: &'a ProviderId,
    pub(super) role_id: Option<&'a RoleId>,
    pub(super) agent_config: &'a AgentConfig,
}

/// The resolved location and trust of one environment, after worktree selection.
pub(super) struct EnvironmentLocation {
    pub(super) workspace_root: Option<PathBuf>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) isolated: bool,
    pub(super) trusted: bool,
}

/// Runtime capabilities and the matching context to persist before publication.
pub(super) struct PreparedEnvironment {
    pub(super) tool_state: ToolSessionState,
    pub(super) context: PersistedSessionContext,
}

impl SessionEnvironment<'_> {
    pub(super) fn prepare(
        &self,
        location: EnvironmentLocation,
        retained_grants: &[horizon_sandbox::FilesystemGrant],
    ) -> PreparedEnvironment {
        let Self {
            state,
            session_id,
            provider_id,
            role_id,
            agent_config,
        } = *self;
        let EnvironmentLocation {
            workspace_root,
            parent_session_id,
            isolated,
            trusted,
        } = location;
        // Blocks this session's own dedicated thread (never `main`'s accept
        // loop, and never the readiness gate `session_list`/`session_new`
        // block on) until the event-log writer thread's own DuckDB
        // rebuild-or-open decision has landed -- see `AgentdState::
        // wait_for_duckdb_store`'s doc comment.
        let recall = RecallContext {
            session_id: Some(session_id),
            store: state.wait_for_duckdb_store(),
        };
        // Use the same resolved root as the provider's prompt-side skill
        // listing. Otherwise an isolated session can be told that a repository
        // skill exists and then have `skill.read` look for its body in the
        // daemon's checkout instead of the session's worktree (backlog 58).
        let skill_root = skill_discovery_root(workspace_root.as_deref());
        // Leg 4b (`docs/agent-approval-design.md`): the network proxy is now
        // `horizon-agent`'s own responsibility, started per session (never one
        // shared daemon-wide instance -- see `tools::network::
        // SessionNetworkProxy`'s doc comment for the per-session-attribution
        // reasoning) and only when this session could ever actually reach tier
        // 1 -- the exact same `isolated && sandbox_available` precondition
        // `policy::classify_call` gates `bash`'s `Contained` classification on,
        // so a session that could never engage the sandbox never pays for a
        // proxy it will never use. A bind failure is non-fatal: this session
        // just falls back to `NetworkPolicy::Disabled` for tier-1 sandboxed
        // `bash`, exactly the pre-leg-4a behavior.
        //
        // Pre-seeded with the project's `[grants]` `network` domain entries
        // (unifying what used to be a domain-only-at-runtime approval flow
        // with the endpoint-only
        // `loopback_connect` config key into one `network` key): a domain the
        // project already trusts never needs a judge/approval round trip
        // through the proxy below. The runtime grant flow (`tools::approval`'s
        // domain-denial-retry path, `SessionDomainPolicy::allow`) still applies
        // on top for anything not listed here.
        let domains =
            SessionDomainPolicy::with_allowed(configured_domains(state, workspace_root.as_deref()));
        let network = if isolated && horizon_sandbox::is_available() {
            match horizon_agent::tools::SessionNetworkProxy::start_with_policy(&domains) {
                Ok(proxy) => Some(Arc::new(proxy)),
                Err(error) => {
                    eprintln!(
                        "horizon-agentd: failed to start session {session_id:?}'s network-proxy \
                     bridge ({error}); tier-1 sandboxed bash will run with network disabled"
                    );
                    None
                }
            }
        } else {
            None
        };

        // A session's judge retains the auxiliary connection accepted at spawn.
        let judge = JudgeHandle::new(agent_config.auxiliary.as_ref(), state.writer());

        // `task`'s daemon capability (`docs/agent-explore-design.md`).
        // Withheld from an exploration session itself: its role allowlist
        // already omits `task` (decision 4), and withholding the host
        // too means a recursion is impossible rather than merely unadvertised.
        let exploration: Option<Arc<dyn horizon_agent::tools::ExplorationHost>> =
            if role_id.is_some_and(horizon_agent::roles::is_exploration) {
                None
            } else {
                Some(Arc::new(AgentdExplorationHost {
                    state: state.clone(),
                    requester_id: session_id,
                    provider_id: provider_id.clone(),
                    workspace_root: workspace_root.clone(),
                }))
            };

        // `[grants]`, resolved once per session at spawn
        // (`docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
        // decision). Injected into the sandbox policy from the start, so a
        // write inside one of this project's granted trees is simply not a
        // boundary crossing and never reaches the judge or a human. Live
        // sessions are unaffected by later config edits; `Reload Session
        // Runtime` picks changes up for new ones, same lifecycle as
        // `[[providers]]`.
        let mut filesystem_grants = configured_filesystem_grants(state, workspace_root.as_deref());
        for grant in retained_grants {
            if !filesystem_grants.contains(grant) {
                filesystem_grants.push(grant.clone());
            }
        }
        let loopback_connect = configured_loopback_connect(state, workspace_root.as_deref());
        let mach_services = configured_mach_services(state, workspace_root.as_deref());
        // Constructed before `workspace_root` is moved into
        // `tool_session_builder_for` below.
        let board = board_host_for(workspace_root.as_deref(), state.clone());
        let tool_state = tool_session_builder_for(workspace_root, agent_config.tools, recall)
            .with_isolated_worktree(isolated)
            // An explore-role session (`task` children and Mixture-of-Agents
            // proposers) is never attached to a pane, so an approval prompt
            // raised for it would reach nobody.
            .with_unattended(role_id.is_some_and(horizon_agent::roles::is_exploration))
            .with_filesystem_grants(filesystem_grants.clone())
            .with_loopback_connect(loopback_connect)
            .with_skills(if trusted {
                SkillRegistry::discover(&skill_root)
            } else {
                SkillRegistry::embedded()
            })
            .with_config_path(state.config_path.clone())
            .with_mach_services(mach_services)
            .with_domain_policy(domains)
            .with_network_proxy(network)
            .with_judge(judge)
            .with_exploration_host(exploration)
            .build()
            .with_board_host(board);
        let persisted_context = PersistedSessionContext {
            workspace_root: tool_state.workspace_root().map(Path::to_path_buf),
            isolated_worktree: isolated,
            parent_session_id: isolated.then_some(parent_session_id).flatten(),
            // What authority this session actually started with. A grant
            // approved later restates this on every subsequent record (see
            // `LiveState::record_filesystem_grants`).
            filesystem_grants,
        };
        PreparedEnvironment {
            tool_state,
            context: persisted_context,
        }
    }

    /// Keep the lifecycle gate through persistence, publication, and rollback;
    /// acknowledge the provider only after the replacement is ready.
    pub(super) fn activate(
        &self,
        base: &str,
        live_state: &LiveState,
        tool_state: &mut ToolSessionState,
        results: &Sender<ToolCompletion>,
        commands: &Sender<Command>,
    ) {
        let state = self.state;
        let session_id = self.session_id;
        let prepared = (|| {
            let _lifecycle = lock_unpoisoned(&state.lifecycle);
            let writer = state
                .writer()
                .ok_or("Environment activation requires persistence")?;
            writer.flush().map_err(|error| error.to_string())?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !horizon_agent::tools::session_tool_work_settled(session_id) {
                if std::time::Instant::now() >= deadline {
                    return Err(
                        "Previous tool work is still settling; retry activation after it finishes"
                            .into(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let root = tool_state
                .workspace_root()
                .ok_or("Session has no repository root")?;
            if lock_unpoisoned(&state.sessions)
                .get(&session_id)
                .is_some_and(|entry| entry.worktree.is_some())
            {
                return Err("Session already owns a worktree".to_string());
            }
            let retained_grants = tool_state.retained_filesystem_grants();
            for grant in &retained_grants {
                horizon_sandbox::revalidate_grant(grant).map_err(|error| error.to_string())?;
            }
            let worktree =
                crate::worktree::create_isolated_worktree_at(root, session_id.as_uuid(), base)?;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let trusted = project_is_trusted(state, Some(&worktree.path));
                let PreparedEnvironment {
                    tool_state: replacement,
                    context,
                } = self.prepare(
                    EnvironmentLocation {
                        workspace_root: Some(worktree.path.clone()),
                        parent_session_id: None,
                        isolated: true,
                        trusted,
                    },
                    &retained_grants,
                );
                let identity = worktree.identity();
                live_state
                    .activate_context(context, Event::EnvironmentActivated(identity.clone()))?;
                register_session_runtime(
                    session_id,
                    replacement.clone(),
                    live_state.clone(),
                    results.clone(),
                );
                register_exploration_host(session_id, replacement.exploration_host());
                *tool_state = replacement;
                if let Some(entry) = lock_unpoisoned(&state.sessions).get_mut(&session_id) {
                    entry.workspace_root = Some(worktree.path.clone());
                    entry.worktree = Some(worktree.clone());
                }
                send_session_event(
                    state,
                    session_id,
                    AgentWireEvent::Event(Event::EnvironmentActivated(identity)),
                );
                Ok((worktree.path.clone(), trusted))
            }))
            .unwrap_or_else(|_| Err("Failed to prepare the session environment".into()));
            if outcome.is_err() {
                crate::worktree::remove_worktree_if_clean(&worktree);
            }
            outcome
        })();
        let command = match prepared {
            Ok((workspace_root, trusted_project)) => Command::EnvironmentPrepared {
                workspace_root,
                trusted_project,
            },
            Err(message) => Command::EnvironmentActivationFailed { message },
        };
        let _ = commands.send(command);
    }
}

#[cfg(test)]
mod tests;
