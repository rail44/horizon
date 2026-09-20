//! The session loop itself: everything from `Initialize` to the provider's
//! channel closing, running synchronously on the session's own thread.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::contract::{
    self, Command, Error as AgentError, Event, Initialization, ProviderEvent, ProviderId, SessionId,
};
use horizon_agent::judge::JudgeHandle;
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::PersistedSessionContext;
use horizon_agent::roles::RoleId;
use horizon_agent::skills::SkillRegistry;
use horizon_agent::tools::{
    process_agent_provider_event, register_exploration_host, register_session_runtime, HostTools,
    RecallContext, SessionDomainPolicy, ToolCompletion, ToolSessionState,
};
use horizon_agent::wire::AgentWireEvent;

use super::approval::{dispatch_inbound_command, gate_processing_approval};
use super::board::board_host_for;
use super::completion::fold_tool_completion;
use super::events::send_session_event;
use super::exploration::AgentdExplorationHost;
use super::host_tools::AgentdHostTools;
use super::panic::{
    catch_session_panic, record_session_loop_panic, record_unexpected_provider_exit,
    SessionLoopPhase,
};
use super::setup::{
    configured_domains, configured_filesystem_grants, configured_loopback_connect,
    configured_mach_services, project_is_trusted, resolve_and_create_isolated_worktree,
    skill_discovery_root, tool_session_state_for,
};
use super::state::{lock_unpoisoned, AgentdState};
use crate::worktree::WorktreeInfo;

/// The session's whole lifetime, from `Initialize` through to the
/// provider's channel closing. Runs entirely synchronously on its own
/// dedicated thread -- see the module doc for why. Faithfully mirrors the
/// deleted in-process agent runtime's shape, minus the floem signals/
/// effects it used to fold through: register the tool/live state (seeded with
/// `history`, see [`super::resume::resume_persisted_sessions`]), send `Initialize`, then
/// fold every provider event / bash completion / inbound command / replay
/// request as it arrives, forwarding the resulting (non-ephemeral) events to
/// Horizon over the wire exactly as `LiveState::extend_provider_events`
/// folded them in-process.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_session(
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    restored_worktree: Option<WorktreeInfo>,
    state: &Arc<AgentdState>,
    inbound_rx: Receiver<Command>,
    replay_rx: Receiver<Sender<Vec<Event>>>,
    history: Vec<Event>,
    phase: &Cell<SessionLoopPhase>,
    retained_grants: Vec<horizon_sandbox::FilesystemGrant>,
) {
    // Resolved *before* starting the provider session (below) so the real,
    // post-isolation root -- an isolated worktree, when this session is
    // isolated, not merely the pre-isolation `workspace_root` this function
    // was called with -- can be threaded straight into `start_session`'s new
    // `workspace_root` argument (`contract::StartSession::workspace_root`'s
    // doc comment): the rig provider builds its system prompt's environment
    // (and the prompt's skills listing, `providers::rig::session::
    // session_extra_sections`) from exactly that value, once, at session
    // spawn time. Later activation uses the explicit environment handshake. This was
    // the 2026-07-19 dogfooding bug: an isolated session's prompt claimed the
    // daemon's own cwd as its working directory, so the model tried to write
    // files into the root checkout instead of its own worktree. Doing this
    // ahead of the provider/role validation just below means a (rare --
    // effectively never in production, see that check's own doc comment)
    // unknown provider or role pays for an isolated worktree's creation and
    // immediate teardown; `spawn_session_thread`'s post-`run_session`
    // cleanup removes it regardless of how this function returns, so
    // nothing is leaked.
    let (workspace_root, isolated) = if let Some(worktree) = restored_worktree {
        (Some(worktree.path), true)
    } else if isolate {
        resolve_and_create_isolated_worktree(
            state,
            session_id,
            spawn_source_session_id,
            workspace_root,
        )
    } else {
        (workspace_root, false)
    };

    // Repository-trust gate: resolved from the
    // same post-isolation `workspace_root` as `[grants]` (so an isolated
    // worktree session inherits its parent project's trust), using the same
    // `worktree::project_root` resolution. Threaded into the provider so
    // `session_extra_sections` can gate skills/instructions, and used below
    // to gate the tool-side skill registry.
    let trusted = project_is_trusted(state, workspace_root.as_deref());

    let handle = {
        let providers = lock_unpoisoned(&state.providers);
        providers.start_session(
            &provider_id,
            session_id,
            role_id.clone(),
            workspace_root.clone(),
            history.clone(),
            trusted,
        )
    };
    let Some(handle) = handle else {
        // `ProviderRegistry::start_session` returns `None` for either an
        // unknown `provider_id` or an unresolvable `role_id` (see its own
        // doc comment on why role validation is centralized there) -- this
        // is `roles`'s "never silently degrade to role-less" requirement's
        // one production enforcement point, so the message distinguishes
        // which one actually failed rather than defaulting to a generic
        // "unknown provider" that would be misleading for a bad role.
        let message = match &role_id {
            Some(role_id) if horizon_agent::roles::resolve(role_id).is_none() => {
                format!("Unknown role `{}`.", role_id.0)
            }
            _ => format!("Unknown provider `{}`.", provider_id.0),
        };
        send_session_event(
            state,
            session_id,
            AgentWireEvent::Event(Event::Error(AgentError { message })),
        );
        return;
    };

    let agent_config = lock_unpoisoned(&state.agent_config).clone();
    let (mut tool_state, persisted_context) = prepare_environment(
        state,
        session_id,
        &provider_id,
        role_id.as_ref(),
        workspace_root,
        spawn_source_session_id,
        isolated,
        trusted,
        &agent_config,
        &retained_grants,
    );
    let live_state = match state.writer() {
        Some(writer) => LiveState::with_event_log_context_and_history(
            session_id,
            Some(provider_id.clone()),
            role_id.clone(),
            writer,
            Some(persisted_context.clone()),
            history,
        ),
        None => LiveState::with_disabled_persistence(),
    };
    if isolated
        && !live_state
            .events()
            .iter()
            .any(|event| matches!(event, Event::EnvironmentActivated(_)))
    {
        let identity = lock_unpoisoned(&state.sessions)
            .get(&session_id)
            .and_then(|entry| entry.worktree.as_ref().map(WorktreeInfo::identity));
        if let Some(identity) = identity {
            if let Err(error) = live_state
                .activate_context(persisted_context, Event::EnvironmentActivated(identity))
            {
                eprintln!("horizon-agentd: could not retain session worktree identity: {error}");
            }
        }
    }
    let (async_results_tx, async_results_rx) = unbounded::<ToolCompletion>();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        live_state.clone(),
        async_results_tx.clone(),
    );
    // The session-runtime registry is thread-local to this thread; the rig
    // session loop runs on its own and reaches the spawn capability through
    // this process-global one instead.
    register_exploration_host(session_id, tool_state.exploration_host());

    let host = AgentdHostTools {
        state: state.clone(),
    };

    let commands_tx = handle.sender();
    let _ = commands_tx.send(Command::Initialize(Initialization {
        session_id,
        provider_id: provider_id.clone(),
        role_id: role_id.clone(),
    }));

    let provider_events = handle.events();

    let loop_outcome = catch_session_panic(phase, || loop {
        phase.set(SessionLoopPhase::WaitingForInput);
        crossbeam_channel::select! {
            recv(provider_events) -> message => match message {
                Ok(provider_event) => {
                    phase.set(SessionLoopPhase::ProviderEvent(contract::event_kind(
                        &provider_event.event,
                    )));
                    if let Event::EnvironmentReady { base } = &provider_event.event {
                        activate_environment(state, session_id, &provider_id, role_id.as_ref(), base, &agent_config, &live_state, &mut tool_state, &async_results_tx, &commands_tx);
                        continue;
                    }
                    handle_provider_event(
                        &host,
                        state,
                        &tool_state,
                        &live_state,
                        &commands_tx,
                        &inbound_rx,
                        session_id,
                        provider_event,
                    );
                }
                Err(_) => {
                    record_unexpected_provider_exit(state, &live_state, session_id);
                    break;
                }
            },
            recv(async_results_rx) -> message => {
                if let Ok(completion) = message {
                    phase.set(SessionLoopPhase::ToolCompletion);
                    // Inputs already waiting at this tool boundary must reach
                    // the provider before its result starts the next round.
                    for command in inbound_rx.try_iter() {
                        dispatch_inbound_command(state, &live_state, &commands_tx, session_id, command);
                    }
                    fold_tool_completion(
                        state,
                        &live_state,
                        &commands_tx,
                        session_id,
                        completion,
                    );
                }
            },
            recv(inbound_rx) -> message => match message {
                Ok(command) => {
                    phase.set(SessionLoopPhase::InboundCommand);
                    dispatch_inbound_command(
                        state,
                        &live_state,
                        &commands_tx,
                        session_id,
                        command,
                    );
                }
                Err(_) => break,
            },
            recv(replay_rx) -> message => {
                if let Ok(reply_tx) = message {
                    phase.set(SessionLoopPhase::Replay);
                    let _ = reply_tx.send(live_state.events());
                }
            },
        }
    });

    if let Err(failure) = loop_outcome {
        eprintln!(
            "horizon-agentd: panic in session {session_id:?}: {}",
            failure.message()
        );
        phase.set(SessionLoopPhase::RecordingPanic);
        record_session_loop_panic(state, &live_state, session_id, &failure);
    }
    let unfinished = super::resume::interrupted_input_outcomes(&live_state.events());
    for event in unfinished {
        if let Event::InputOutcome(mut outcome) = event {
            outcome.outcome = contract::InputResult::Failure {
                message: "Session runtime ended before completing this input.".into(),
            };
            let event = Event::InputOutcome(outcome);
            if live_state
                .persist_provider_events([event.clone().into()])
                .is_ok()
            {
                if let Some(writer) = state.writer() {
                    if writer.flush().is_ok() {
                        send_session_event(state, session_id, AgentWireEvent::Event(event));
                    }
                }
            }
        }
    }
}

/// One provider event through the same processing pipeline the deleted
/// in-process agent runtime's effect used to run
/// (`process_agent_provider_event` for tool execution/policy mapping, then
/// `LiveState::extend_provider_events` for the fold/persist) -- except the
/// resulting frame isn't published to a local `Frames` signal, it's
/// forwarded to Horizon as event envelopes. Ephemeral tool-call progress
/// (`ProviderEvent::tool_call_progress`) is folded into the local frame (so
/// a later `resolve_approval`'s `frame.tool_call_request` lookup stays
/// correct) exactly like every other event, but forwarded as its own
/// `Control::ToolCallProgress` message rather than a `contract::Event` --
/// there's no `Event` variant for it (it's never part of conversation
/// history or the persisted log; see `ToolCallProgress`'s own doc comment),
/// so wrapping it in `Envelope::event` isn't an option. This restores the
/// streaming-tool-call-argument-preview feature the module's step 3 notes in
/// `docs/agent-runtime-split-design.md` recorded as trimmed for agentd mode.
/// `process_agent_provider_event` never mixes progress and real events in
/// one `Processing` (a progress tick always comes back alone), so splitting
/// `horizon_events` into the two forwarding shapes below is exhaustive in
/// practice, not just by construction.
#[allow(clippy::too_many_arguments)]
fn handle_provider_event(
    host: &dyn HostTools,
    state: &Arc<AgentdState>,
    tool_state: &ToolSessionState,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    inbound_rx: &Receiver<Command>,
    session_id: SessionId,
    provider_event: ProviderEvent,
) {
    let mut processing = process_agent_provider_event(host, tool_state, session_id, provider_event);
    gate_processing_approval(session_id, &mut processing.horizon_events);

    let mut to_forward: Vec<AgentWireEvent> = Vec::new();
    for event in &processing.horizon_events {
        match &event.tool_call_progress {
            Some(progress) => to_forward.push(AgentWireEvent::ToolCallProgress(progress.clone())),
            None => to_forward.push(AgentWireEvent::Event(event.event.clone())),
        }
    }
    let durable_result = processing.horizon_events.iter().any(|event| {
        matches!(
            event.event,
            Event::InputOutcome(_) | Event::SessionInputSent { .. }
        )
    });
    if durable_result {
        if live_state
            .persist_provider_events(processing.horizon_events)
            .is_err()
        {
            return;
        }
        let Some(writer) = state.writer() else {
            return;
        };
        if writer.flush().is_err() {
            return;
        }
    } else {
        let _ = live_state.extend_provider_events(processing.horizon_events);
    }
    for event in to_forward {
        send_session_event(state, session_id, event);
    }
    // A synchronous tool can enqueue commands to its own session (notably
    // environment activation). Forward those before its result releases the
    // next provider round; the outer select does not guarantee that ordering.
    // Keep the normal dispatch path so identified inputs still persist first.
    for command in inbound_rx.try_iter() {
        dispatch_inbound_command(state, live_state, commands_tx, session_id, command);
    }
    // A synchronous tool result must not release the next provider decision
    // before its source-send outbox records are acknowledged above.
    for command in processing.provider_commands {
        let _ = commands_tx.send(command);
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_environment(
    state: &Arc<AgentdState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolated: bool,
    trusted: bool,
    agent_config: &horizon_agent::config::AgentConfig,
    retained_grants: &[horizon_sandbox::FilesystemGrant],
) -> (ToolSessionState, PersistedSessionContext) {
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

    // Enforcing judge (`docs/agent-approval-design.md`'s "Judge design"): a
    // second model id on this process's *same* provider/`base_url`, reusing
    // the process's own event-log writer for the verdict record. `None`
    // preserves human approval whenever `OPENAI_API_KEY` isn't set or no
    // writer is configured -- see `JudgeHandle::new`.
    let judge = JudgeHandle::new(agent_config.rig.base_url.clone(), state.writer());

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
    // `[provider]`.
    let mut filesystem_grants = configured_filesystem_grants(state, workspace_root.as_deref());
    for grant in retained_grants {
        if !filesystem_grants.contains(grant) {
            filesystem_grants.push(grant.clone());
        }
    }
    let loopback_connect = configured_loopback_connect(state, workspace_root.as_deref());
    let mach_services = configured_mach_services(state, workspace_root.as_deref());
    // Constructed before `workspace_root` is moved into
    // `tool_session_state_for` below.
    let board = board_host_for(workspace_root.as_deref(), state.clone());
    let tool_state = tool_session_state_for(workspace_root, agent_config.tools, recall)
        .with_isolated_worktree(isolated)
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
        .with_board_host(board);
    let persisted_context = PersistedSessionContext {
        workspace_root: tool_state.workspace_root().map(Path::to_path_buf),
        isolated_worktree: isolated,
        parent_session_id: isolated.then_some(spawn_source_session_id).flatten(),
        // What authority this session actually started with. A grant
        // approved later restates this on every subsequent record (see
        // `LiveState::record_filesystem_grants`).
        filesystem_grants,
    };
    (tool_state, persisted_context)
}

#[allow(clippy::too_many_arguments)]
fn activate_environment(
    state: &Arc<AgentdState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
    base: &str,
    agent_config: &horizon_agent::config::AgentConfig,
    live_state: &LiveState,
    tool_state: &mut ToolSessionState,
    results: &Sender<ToolCompletion>,
    commands: &Sender<Command>,
) {
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
            let (replacement, context) = prepare_environment(
                state,
                session_id,
                provider_id,
                role_id,
                Some(worktree.path.clone()),
                None,
                true,
                trusted,
                agent_config,
                &retained_grants,
            );
            let identity = worktree.identity();
            live_state.activate_context(context, Event::EnvironmentActivated(identity.clone()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_queued_activation_reaches_the_provider_before_its_result() {
        struct QueuingHost(Sender<Command>);
        impl HostTools for QueuingHost {
            fn execute_auto(&self, tool: &str, _: &serde_json::Value) -> Option<serde_json::Value> {
                assert_eq!(tool, "workspace.snapshot");
                self.0
                    .send(Command::ActivateWorktree {
                        base: "selected-base".into(),
                    })
                    .unwrap();
                Some(serde_json::json!({"requested": true}))
            }
        }

        let state = super::super::test_support::judge_test_state();
        let config = lock_unpoisoned(&state.agent_config).clone();
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let (tools, _) = prepare_environment(
            &state,
            session_id,
            &ProviderId("builtin.agent.mock".into()),
            None,
            Some(root.path().to_path_buf()),
            None,
            false,
            false,
            &config,
            &[],
        );
        let (inbound_tx, inbound_rx) = unbounded();
        let (provider_tx, provider_rx) = unbounded();
        let call_id = contract::ToolCallId("activation-trigger".into());
        handle_provider_event(
            &QueuingHost(inbound_tx),
            &state,
            &tools,
            &LiveState::with_disabled_persistence(),
            &provider_tx,
            &inbound_rx,
            session_id,
            Event::ToolCallRequested(contract::ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace.snapshot".into(),
                input: serde_json::json!({}).into(),
                occurrence_id: None,
            })
            .into(),
        );
        assert_eq!(
            provider_rx.try_recv().unwrap(),
            Command::ActivateWorktree {
                base: "selected-base".into()
            }
        );
        assert!(
            matches!(provider_rx.try_recv().unwrap(), Command::ToolCallResult(result) if result.call_id == call_id)
        );
        assert!(provider_rx.try_recv().is_err());
    }

    #[test]
    fn environment_rebuild_retains_additional_grants_without_granting_the_old_root() {
        let dirs = tempfile::tempdir().unwrap();
        let old_root = dirs.path().join("consultation");
        let new_root = dirs.path().join("implementation");
        let dependency = dirs.path().join("dependency");
        for root in [&old_root, &new_root, &dependency] {
            std::fs::create_dir(root).unwrap();
        }
        let grant = horizon_sandbox::FilesystemGrant {
            path: dependency.canonicalize().unwrap(),
            access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
            scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: vec![],
        };
        let state = super::super::test_support::judge_test_state();
        let config = lock_unpoisoned(&state.agent_config).clone();
        let (tools, context) = prepare_environment(
            &state,
            SessionId::new(),
            &ProviderId("builtin.agent.mock".into()),
            None,
            Some(new_root.clone()),
            None,
            false,
            false,
            &config,
            std::slice::from_ref(&grant),
        );
        assert_eq!(tools.workspace_root(), Some(new_root.as_path()));
        assert_eq!(
            tools.retained_filesystem_grants(),
            std::slice::from_ref(&grant)
        );
        assert_eq!(context.filesystem_grants, [grant]);
        assert!(!context
            .filesystem_grants
            .iter()
            .any(|grant| grant.path == old_root));
    }
}
