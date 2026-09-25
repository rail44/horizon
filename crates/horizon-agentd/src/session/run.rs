//! The session loop itself: everything from `Initialize` to the provider's
//! channel closing, running synchronously on the session's own thread.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::contract::{
    self, Command, Error as AgentError, Event, Initialization, ProviderEvent, ProviderId, SessionId,
};
use horizon_agent::live::LiveState;
use horizon_agent::roles::RoleId;
use horizon_agent::tools::{
    process_agent_provider_event, register_exploration_host, register_session_runtime, HostTools,
    ToolCompletion, ToolSessionState,
};
use horizon_agent::wire::AgentWireEvent;

use super::approval::{dispatch_inbound_command, gate_processing_approval};
use super::completion::fold_tool_completion;
use super::environment::{EnvironmentLocation, PreparedEnvironment, SessionEnvironment};
use super::events::{
    persist_and_send_session_event, report_persistence_failure, send_session_event,
};
use super::host_tools::AgentdHostTools;
use super::panic::{
    catch_session_panic, record_session_loop_panic, record_unexpected_provider_exit,
    SessionLoopPhase,
};
use super::setup::{project_is_trusted, resolve_and_create_isolated_worktree};
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
    let environment = SessionEnvironment {
        state,
        session_id,
        provider_id: &provider_id,
        role_id: role_id.as_ref(),
        agent_config: &agent_config,
    };
    let PreparedEnvironment {
        mut tool_state,
        context: persisted_context,
    } = environment.prepare(
        EnvironmentLocation {
            workspace_root,
            parent_session_id: spawn_source_session_id,
            isolated,
            trusted,
        },
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
                report_persistence_failure(state, &live_state, session_id, error);
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

    let mut provider_events = handle.events();
    let failure_subscription = state.writer().map(|writer| writer.subscribe_failure());
    let mut failure_rx = failure_subscription
        .as_ref()
        .map(|subscription| subscription.receiver().clone())
        .unwrap_or_else(crossbeam_channel::never);
    let mut stopped = false;

    let loop_outcome = catch_session_panic(phase, || loop {
        if !stopped {
            if let Some(message) = live_state.persistence_failure() {
                report_persistence_failure(state, &live_state, session_id, message);
                let _ = commands_tx.send(Command::Shutdown);
                provider_events = crossbeam_channel::never();
                failure_rx = crossbeam_channel::never();
                stopped = true;
            }
        }
        phase.set(SessionLoopPhase::WaitingForInput);
        crossbeam_channel::select! {
            recv(failure_rx) -> message => {
                if let Ok(message) = message { report_persistence_failure(state, &live_state, session_id, message); }
            },
            recv(provider_events) -> message => match message {
                Ok(provider_event) => {
                    phase.set(SessionLoopPhase::ProviderEvent(provider_event.kind()));
                    if let Some(Event::EnvironmentReady { base }) = provider_event.as_event() {
                        environment.activate(base, &live_state, &mut tool_state, &async_results_tx, &commands_tx);
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
                    if stopped { continue; }
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
                    if stopped {
                        if matches!(command, Command::Shutdown) { break; }
                        continue;
                    }
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
                    let _ = reply_tx.send(live_state.replay_events());
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
            persist_and_send_session_event(state, &live_state, session_id, event);
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
/// Feedback keeps its dedicated wire variant through the shared conversion;
/// it cannot also carry a conversation event.
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
    if let Some(Event::ToolCallFinished(result)) = provider_event.as_event() {
        if !horizon_agent::tools::ToolCompletion::Finished(result.clone())
            .matches_live_request(&live_state.frame())
        {
            return;
        }
    }
    if !super::events::execution_available(state, live_state, session_id) {
        return;
    }
    let mut processing = match process_agent_provider_event(
        host,
        tool_state,
        session_id,
        live_state,
        provider_event,
    ) {
        Ok(processing) => processing,
        Err(message) => {
            report_persistence_failure(state, live_state, session_id, message);
            let _ = commands_tx.send(Command::Shutdown);
            return;
        }
    };
    if let Some(candidate) = processing.approval.take() {
        let (events, commands) = gate_processing_approval(tool_state, session_id, candidate);
        if let Err(message) = live_state.extend_provider_events(events.clone()) {
            report_persistence_failure(state, live_state, session_id, message);
            let _ = commands_tx.send(Command::Shutdown);
            return;
        }
        processing.horizon_events.extend(events);
        processing.provider_commands.extend(commands);
    }
    for event in processing.horizon_events {
        super::model_selection::record_applied(state, session_id, &event);
        send_session_event(state, session_id, AgentWireEvent::from(&event));
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
    if !super::events::execution_available(state, live_state, session_id) {
        return;
    }
    for command in processing.provider_commands {
        let _ = commands_tx.send(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_request_persistence_prevents_tool_execution_and_provider_release() {
        struct MustNotRun;
        impl HostTools for MustNotRun {
            fn execute_auto(&self, _: &str, _: &serde_json::Value) -> Option<serde_json::Value> {
                panic!("tool ran after its request failed to persist");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let state = super::super::test_support::test_state();
        let session_id = SessionId::new();
        let (writer, ready) = horizon_agent::persistence::event_log::WriterHandle::open(dir.path());
        assert!(matches!(
            ready.recv().unwrap(),
            horizon_agent::persistence::event_log::WriterInit::Failed(_)
        ));
        let live =
            LiveState::with_event_log_and_history(session_id, None, None, writer, Vec::new());
        let mut published =
            super::super::Connection::new(state.clone()).subscribe_agent(session_id);
        let (_input, inbound) = unbounded();
        let (commands, output) = unbounded();
        handle_provider_event(
            &MustNotRun,
            &state,
            &ToolSessionState::for_root(
                dir.path().to_path_buf(),
                Default::default(),
                Default::default(),
            ),
            &live,
            &commands,
            &inbound,
            session_id,
            Event::ToolCallRequested(contract::ToolCallRequest {
                call_id: contract::ToolCallId("blocked".into()),
                occurrence_id: contract::OccurrenceId::new(),
                tool_id: "workspace.snapshot".into(),
                input: serde_json::json!({}).into(),
            })
            .into(),
        );
        assert!(live.events().is_empty());
        assert!(output.try_recv().is_err());
        let events = super::super::test_support::drain_events(&mut published);
        assert!(matches!(
            events.as_slice(),
            [
                Event::Error(_),
                Event::StateChanged(contract::SessionState::Terminated)
            ]
        ));
        assert_eq!(live.replay_events(), events);
    }

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

        let state = super::super::test_support::test_state();
        let config = lock_unpoisoned(&state.agent_config).clone();
        let root = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let tools = SessionEnvironment {
            state: &state,
            session_id,
            provider_id: &ProviderId("builtin.agent.mock".into()),
            role_id: None,
            agent_config: &config,
        }
        .prepare(
            EnvironmentLocation {
                workspace_root: Some(root.path().to_path_buf()),
                parent_session_id: None,
                isolated: false,
                trusted: false,
            },
            &[],
        )
        .tool_state;
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
                occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
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
}
