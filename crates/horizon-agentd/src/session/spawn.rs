//! Spawning one session's dedicated OS thread and registering it, plus the
//! model resolution announced at that moment.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::unbounded;

use horizon_agent::contract::{Command, Event, ProviderId, SessionId};
use horizon_agent::roles::RoleId;
use horizon_agent::runtime_panic::catch_runtime_panic;
use horizon_agent::tools::unregister_session_runtime;
use horizon_agent::wire::AgentWireEvent;

use super::events::send_session_event;
use super::panic::{
    catch_session_panic, record_uncaught_session_panic, SessionLoopPhase, SessionPanic,
};
use super::run::run_session;
use super::state::{lock_unpoisoned, AgentdState, SessionEntry};
use crate::worktree::{self, WorktreeInfo};

/// Resolves this session's model (pure and synchronous -- see
/// `Provider::resolved_model`'s doc comment) and, if resolvable, announces
/// it live to whichever client is connected right now, if any. Pulled out
/// of [`spawn_session_thread`] as its own function purely so this
/// resolve-then-maybe-send step is unit-testable without spinning up a
/// whole session thread -- same reason [`super::setup::tool_session_state_for`] was.
///
/// A fresh `Control::SessionNew` caller is already listening
/// (`AgentdHandle::start_session` registers the session's route before
/// sending `SessionNew`), so it sees this immediately; a resumed session
/// spawned at daemon startup usually has no connection yet
/// ([`send_session_event`] silently drops it then) -- [`super::attachment::capture`]
/// re-announces the same value for that case, from `Control::SessionLoad`'s
/// handler. See `docs/agent-output-ui-amendment.md`'s dated model-chip
/// addendum.
fn resolve_and_announce_session_model(
    state: &Arc<AgentdState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
) -> (Option<String>, Option<horizon_agent::wire::ModelSelection>) {
    let model = lock_unpoisoned(&state.providers).resolved_model(provider_id, role_id);
    let selection = model
        .as_deref()
        .and_then(|model| selection_for_provider_id(state, provider_id, model));
    if let Some(model) = &model {
        send_session_event(
            state,
            session_id,
            AgentWireEvent::SessionModel(model.clone()),
        );
    }
    if let Some(selection) = &selection {
        send_session_event(
            state,
            session_id,
            AgentWireEvent::SessionSelection(selection.clone()),
        );
    }
    (model, selection)
}

/// The `(provider, model)` display selection for a session spawned directly on
/// `provider_id`, in the vocabulary the picker and `set_session_model` use: the
/// config entry name (the built-in default resolves as `default`)
/// plus the model the session runs. `None` for an id with no config entry (e.g.
/// the mock provider), where the chip falls back to the resolved model id.
fn selection_for_provider_id(
    state: &Arc<AgentdState>,
    provider_id: &ProviderId,
    model: &str,
) -> Option<horizon_agent::wire::ModelSelection> {
    let id = provider_id.0.as_str();
    let config = lock_unpoisoned(&state.agent_config);
    if id == "builtin.agent.rig" {
        return Some(horizon_agent::wire::ModelSelection {
            provider: config.providers.default_name.clone(),
            model: model.to_string(),
        });
    }
    if let Some(name) = id.strip_prefix("builtin.agent.rig.") {
        if config.providers.entry(name).is_some() {
            return Some(horizon_agent::wire::ModelSelection {
                provider: name.to_string(),
                model: model.to_string(),
            });
        }
        return None;
    }
    if let Some(name) = id.strip_prefix("builtin.agent.moa.") {
        return Some(horizon_agent::wire::ModelSelection {
            provider: horizon_agent::config::MOA_PROVIDER_NAME.to_string(),
            model: name.to_string(),
        });
    }
    None
}

/// Spawns the dedicated thread for one session — the shared spawn path for
/// both a fresh `Control::SessionNew` ([`super::connection::Connection::handle_session_new`])
/// and a session resumed from the persisted log at startup
/// ([`super::resume::resume_persisted_sessions`]); `history` is empty for the former,
/// already-committed events for the latter. A resumed isolated session passes
/// `restored_worktree` only after [`worktree::adopt_isolated_worktree`]
/// recomputes and validates its Git/path relationships; it never asks the
/// fresh-spawn `isolate` path to create a second worktree.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_session_thread(
    state: Arc<AgentdState>,
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    restored_worktree: Option<WorktreeInfo>,
    history: Vec<Event>,
) {
    spawn_session_thread_with_context(
        state,
        session_id,
        provider_id,
        role_id,
        workspace_root,
        spawn_source_session_id,
        isolate,
        restored_worktree,
        history,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_session_thread_with_context(
    state: Arc<AgentdState>,
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    restored_worktree: Option<WorktreeInfo>,
    history: Vec<Event>,
    retained_context: Option<horizon_agent::persistence::event_log::PersistedSessionContext>,
) {
    if let Some(message) = state.writer().and_then(|writer| writer.failure()) {
        super::events::report_persistence_failure(
            &state,
            &horizon_agent::live::LiveState::with_disabled_persistence(),
            session_id,
            message,
        );
        return;
    }
    let (inbound_tx, inbound_rx) = unbounded::<Command>();
    let (replay_tx, replay_rx) = unbounded::<crate::session::attachment::AttachRequest>();
    let (model, selection) =
        resolve_and_announce_session_model(&state, session_id, &provider_id, role_id.as_ref());
    let restored_root = restored_worktree
        .as_ref()
        .map(|worktree| worktree.path.clone())
        .or_else(|| workspace_root.clone());
    state.sessions.lock().unwrap().insert(
        session_id,
        SessionEntry {
            provider_id: provider_id.clone(),
            role_id: role_id.clone(),
            model,
            selection,
            inbound: inbound_tx,
            replay: replay_tx,
            parent_session_id: restored_worktree.as_ref().and(spawn_source_session_id),
            workspace_root: restored_root,
            worktree: restored_worktree.clone(),
        },
    );

    let thread_state = state.clone();
    let panic_provider_id = provider_id.clone();
    let panic_role_id = role_id.clone();
    thread::spawn(move || {
        let phase = Cell::new(SessionLoopPhase::Starting);
        let outcome = catch_session_panic(&phase, || {
            run_session(
                session_id,
                provider_id,
                role_id,
                workspace_root,
                spawn_source_session_id,
                isolate,
                restored_worktree,
                &thread_state,
                inbound_rx,
                replay_rx,
                history,
                &phase,
                retained_context
                    .map(|context| context.filesystem_grants)
                    .unwrap_or_default(),
            );
        });
        if let Err(failure) = outcome {
            eprintln!(
                "horizon-agentd: uncaught panic in session {session_id:?}: {}",
                failure.message()
            );
            phase.set(SessionLoopPhase::RecordingPanic);
            let report_outcome = catch_runtime_panic(|| {
                record_uncaught_session_panic(
                    &thread_state,
                    session_id,
                    &panic_provider_id,
                    panic_role_id.as_ref(),
                    &failure,
                );
            });
            if let Err(report) = report_outcome {
                let reporting_failure = SessionPanic::from_report(phase.get(), report);
                eprintln!(
                    "horizon-agentd: could not record panic for session {session_id:?}: {}",
                    reporting_failure.message()
                );
            }
        }
        // This thread-local registration must be cleared even when setup or
        // the event loop unwinds. Leaving it until `run_session`'s normal
        // return was the same stale-registration shape as the process-wide
        // session entry fixed below. Cleanup itself gets a final boundary so
        // an unrelated cleanup defect cannot skip removal from `sessions`.
        phase.set(SessionLoopPhase::CleaningUp);
        if let Err(report) = catch_runtime_panic(|| {
            unregister_session_runtime(session_id);
        }) {
            let cleanup_failure = SessionPanic::from_report(phase.get(), report);
            eprintln!(
                "horizon-agentd: cleanup panic in session {session_id:?}: {}",
                cleanup_failure.message()
            );
        }
        // Decision 5: a session that owned an isolated worktree gets it
        // cleaned up (if clean) exactly when its own thread ends -- which
        // only happens on a genuine `Command::Shutdown`/provider exit (the
        // daemon-side "terminate" signal), never on a mere close/detach
        // (those leave the thread, and this session, running -- see the
        // module doc's "sessions are scoped to the process" note).
        let work_settled =
            horizon_agent::tools::drain_session_work(session_id, std::time::Duration::from_secs(5));
        let _lifecycle = lock_unpoisoned(&thread_state.lifecycle);
        let entry = lock_unpoisoned(&thread_state.sessions).remove(&session_id);
        lock_unpoisoned(&thread_state.agent_subscribers).remove(&session_id);
        if let Some(worktree) = entry.and_then(|entry| entry.worktree) {
            if !work_settled || !worktree::remove_worktree_if_clean(&worktree) {
                eprintln!(
                    "horizon-agentd: kept worktree {} for {session_id:?} (not clean or background work still stopping)",
                    worktree.path.display()
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::state_with_rig_config;
    use crate::session::Connection;

    /// A resolvable model (rig provider, `api_key_present: true`) is both
    /// returned (for `SessionEntry::model`) and announced live as a
    /// session-scoped `Control::SessionModel`, matching how `role_id`
    /// already travels -- see `docs/agent-output-ui-amendment.md`'s dated
    /// model-chip addendum.
    #[test]
    fn resolve_and_announce_session_model_sends_and_returns_the_resolved_model() {
        let state = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
        let provider_id = ProviderId("builtin.agent.rig".to_string());

        let (model, _selection) =
            resolve_and_announce_session_model(&state, session_id, &provider_id, None);

        assert_eq!(model.as_deref(), Some("test-model"));
        let sent = outgoing_rx
            .try_recv()
            .expect("a SessionModel event should have been sent");
        assert!(
            matches!(&sent, AgentWireEvent::SessionModel(model) if model == "test-model"),
            "expected a SessionModel wire event, got: {sent:?}",
        );
    }

    /// Deterministic fallback mode (no `OPENAI_API_KEY`, mirrored here via
    /// `api_key_present: false`) never calls a real provider, so there is no
    /// honest model to report -- nothing must be sent, mirroring
    /// `Control::SkippedLines`'s "omitted entirely" convention.
    #[test]
    fn resolve_and_announce_session_model_sends_nothing_in_deterministic_fallback_mode() {
        let state = state_with_rig_config(false, "test-model");
        let session_id = SessionId::new();
        let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
        let provider_id = ProviderId("builtin.agent.rig".to_string());

        let (model, _selection) =
            resolve_and_announce_session_model(&state, session_id, &provider_id, None);

        assert_eq!(model, None);
        assert!(
            outgoing_rx.try_recv().is_err(),
            "nothing should be sent when there is no resolvable model"
        );
    }
}
