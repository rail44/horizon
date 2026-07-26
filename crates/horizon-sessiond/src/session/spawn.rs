//! Spawning one session's dedicated OS thread and registering it, plus the
//! model resolution announced at that moment.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Sender};

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
use super::state::{lock_unpoisoned, SessionEntry, SessiondState};
use crate::worktree::{self, WorktreeInfo};

/// Resolves this session's model (pure and synchronous -- see
/// `Provider::resolved_model`'s doc comment) and, if resolvable, announces
/// it live to whichever client is connected right now, if any. Pulled out
/// of [`spawn_session_thread`] as its own function purely so this
/// resolve-then-maybe-send step is unit-testable without spinning up a
/// whole session thread -- same reason [`super::setup::tool_session_state_for`] was.
///
/// A fresh `Control::SessionNew` caller is already listening
/// (`SessiondHandle::start_session` registers the session's route before
/// sending `SessionNew`), so it sees this immediately; a resumed session
/// spawned at daemon startup usually has no connection yet
/// ([`send_session_event`] silently drops it then) -- [`super::connection::Connection::session_model`]
/// re-announces the same value for that case, from `Control::SessionLoad`'s
/// handler. See `docs/agent-output-ui-amendment.md`'s dated model-chip
/// addendum.
fn resolve_and_announce_session_model(
    state: &Arc<SessiondState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
) -> Option<String> {
    let model = state.providers.resolved_model(provider_id, role_id);
    if let Some(model) = &model {
        send_session_event(
            state,
            session_id,
            AgentWireEvent::SessionModel(model.clone()),
        );
    }
    model
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
pub(super) fn spawn_session_thread(
    state: Arc<SessiondState>,
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    restored_worktree: Option<WorktreeInfo>,
    history: Vec<Event>,
    // Another session's stream to seed this one's *provider* history from,
    // empty for every session except a fork-seeded exploration -- see
    // `contract::StartSession::seed_history`.
    seed_history: Vec<Event>,
) {
    let (inbound_tx, inbound_rx) = unbounded::<Command>();
    let (replay_tx, replay_rx) = unbounded::<Sender<Vec<Event>>>();
    let model =
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
                seed_history,
                &phase,
            );
        });
        if let Err(failure) = outcome {
            eprintln!(
                "horizon-sessiond: uncaught panic in session {session_id:?}: {}",
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
                    "horizon-sessiond: could not record panic for session {session_id:?}: {}",
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
                "horizon-sessiond: cleanup panic in session {session_id:?}: {}",
                cleanup_failure.message()
            );
        }
        // Decision 5: a session that owned an isolated worktree gets it
        // cleaned up (if clean) exactly when its own thread ends -- which
        // only happens on a genuine `Command::Shutdown`/provider exit (the
        // daemon-side "terminate" signal), never on a mere close/detach
        // (those leave the thread, and this session, running -- see the
        // module doc's "sessions are scoped to the process" note).
        let entry = lock_unpoisoned(&thread_state.sessions).remove(&session_id);
        if let Some(worktree) = entry.and_then(|entry| entry.worktree) {
            if !worktree::remove_worktree_if_clean(&worktree) {
                eprintln!(
                    "horizon-sessiond: kept worktree {} for {session_id:?} (not clean)",
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

    /// A resolvable model (rig provider, `openai_enabled: true`) is both
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

        let model = resolve_and_announce_session_model(&state, session_id, &provider_id, None);

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
    /// `openai_enabled: false`) never calls a real provider, so there is no
    /// honest model to report -- nothing must be sent, mirroring
    /// `Control::SkippedLines`'s "omitted entirely" convention.
    #[test]
    fn resolve_and_announce_session_model_sends_nothing_in_deterministic_fallback_mode() {
        let state = state_with_rig_config(false, "test-model");
        let session_id = SessionId::new();
        let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
        let provider_id = ProviderId("builtin.agent.rig".to_string());

        let model = resolve_and_announce_session_model(&state, session_id, &provider_id, None);

        assert_eq!(model, None);
        assert!(
            outgoing_rx.try_recv().is_err(),
            "nothing should be sent when there is no resolvable model"
        );
    }
}
