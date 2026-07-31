//! Startup resume: turning the persisted event log back into live session
//! threads, and the fixups a restart owes the sessions it finds there.

use std::collections::HashMap;
use std::sync::Arc;

use horizon_agent::contract::{
    Error as AgentError, Event, ProviderEvent, ProviderId, SessionId, SessionState, ToolCallId,
    TurnEndReason,
};
use horizon_agent::frame::{agent_frame_from_events, AgentFrame, AgentFrameItem};
use horizon_agent::persistence::event_log::{
    Appender, PersistedSessionContext, Record, WriterHandle,
};
use horizon_agent::roles::RoleId;
use horizon_agent::tools::cancelled_tool_call_result;

use super::spawn::spawn_session_thread;
use super::state::{lock_unpoisoned, AgentdState};
use crate::worktree;

/// `docs/agent-runtime-split-design.md` step 4, "agentd start": reads the
/// startup read's records and, for each session found (grouped here by
/// `session_id`), resumes it live: any turn still open at that session's
/// tail (`AgentFrame::is_turn_in_flight`, the same "is a turn in flight"
/// check the palette's `Cancel Agent Turn` enablement uses) is committed
/// durably as cancelled *before* the session goes live again, per "any turn
/// open at the log's tail is committed as cancelled" — then a fresh thread
/// is spawned exactly as `Control::SessionNew` would, seeded with the whole
/// history so its first frame is complete. A no-op when there's no writer
/// (persistence disabled for this run — nothing to resume from or write a
/// fixup to).
///
/// Sessions whose log already ends in a terminal state ([`session_is_dead`])
/// are skipped entirely rather than resumed: there is no live provider
/// process left behind a terminated/exited session, so reviving its thread
/// would just leave it parked forever, and doing this for *every* session
/// ever created makes startup cost (and thread count) grow without bound
/// with history -- exactly what was observed as "every historical session
/// comes back as a ghost" before this filter existed. How many sessions hit
/// this skip is counted and reported as one combined summary line after the
/// loop, not printed per session -- a real archived log can carry dozens of
/// long-dead sessions, which used to bury the "resumed session" lines for
/// the ones that actually matter.
pub(crate) fn resume_persisted_sessions(state: &Arc<AgentdState>, records: Vec<Record>) {
    let Some(writer) = state.writer() else {
        return;
    };

    let mut by_session: HashMap<SessionId, Vec<Record>> = HashMap::new();
    for record in records {
        by_session
            .entry(record.session_id)
            .or_default()
            .push(record);
    }

    // Counted rather than printed per session (see the loop below): a real
    // archived log can carry dozens of long-dead sessions, and a line per
    // one drowned out the genuinely interesting "resumed session" lines
    // right next to it.
    let mut skipped_terminated = 0usize;
    // Counted the same way, for the same reason -- see
    // [`terminate_orphaned_exploration`].
    let mut terminated_explorations = 0usize;

    for (session_id, mut session_records) in by_session {
        session_records.sort_by_key(|record| record.sequence);
        let provider_id = session_records
            .iter()
            .rev()
            .find_map(|record| record.provider_id.clone())
            .unwrap_or_else(|| lock_unpoisoned(&state.providers).default_provider_id());
        // Mirrors `provider_id` just above: every record `Appender` writes
        // for a session carries the same `role_id` (see
        // `event_log::Appender::new`), so the last one found scanning from
        // the tail is the session's role for its whole lifetime.
        let role_id = session_records
            .iter()
            .rev()
            .find_map(|record| record.role_id.clone());
        let persisted_context = session_records
            .iter()
            .rev()
            .find_map(|record| record.session_context.clone());
        let (workspace_root, parent_session_id, restored_worktree) =
            match persisted_context.as_ref() {
                Some(context) if context.isolated_worktree => {
                    let Some(root) = context.workspace_root.as_deref() else {
                        eprintln!(
                            "horizon-agentd: refusing to resume isolated session {session_id:?}: \
                             persisted context has no workspace root"
                        );
                        continue;
                    };
                    match worktree::adopt_isolated_worktree(root, session_id.as_uuid()) {
                        Ok(worktree) => (
                            Some(worktree.path.clone()),
                            context.parent_session_id,
                            Some(worktree),
                        ),
                        Err(error) => {
                            eprintln!(
                                "horizon-agentd: refusing to resume isolated session \
                                 {session_id:?}: {error}"
                            );
                            continue;
                        }
                    }
                }
                Some(context) => (context.workspace_root.clone(), None, None),
                // Compatibility for records written before `session_context`
                // existed. They retain the old process-cwd, non-isolated
                // resume behavior; the first new record upgrades them with
                // an explicit context for the following restart.
                None => (None, None, None),
            };
        let mut events: Vec<Event> = session_records
            .into_iter()
            .map(|record| record.event)
            .collect();

        let frame = agent_frame_from_events(&events);
        if session_is_dead(&frame) {
            skipped_terminated += 1;
            continue;
        }

        if role_id
            .as_ref()
            .is_some_and(horizon_agent::roles::is_exploration)
        {
            terminate_orphaned_exploration(
                &writer,
                session_id,
                &provider_id,
                role_id.as_ref(),
                persisted_context.as_ref(),
                &frame,
            );
            terminated_explorations += 1;
            continue;
        }

        if frame.is_turn_in_flight() {
            // Mirrors what a live `Command::Cancel` does (`providers::rig::
            // session`, `providers::mock`): finish every still-outstanding
            // tool call as cancelled *before* the turn-end/state-change
            // pair, so e.g. a call parked in `WaitingForApproval` doesn't
            // keep reading as pending in the resumed frame -- there is no
            // live provider left to eventually answer it.
            let mut closing: Vec<Event> = outstanding_tool_call_ids(&frame)
                .into_iter()
                .map(|call_id| Event::ToolCallFinished(cancelled_tool_call_result(call_id)))
                .collect();
            closing.push(Event::TurnEnded(TurnEndReason::Cancelled));
            closing.push(Event::StateChanged(SessionState::WaitingForUser));

            let mut appender = Appender::new(
                writer.clone(),
                session_id,
                Some(provider_id.clone()),
                role_id.clone(),
            );
            if let Some(context) = persisted_context.clone() {
                appender = appender.with_session_context(context);
            }
            match appender
                .append_provider_events(closing.iter().cloned().map(ProviderEvent::from).collect())
            {
                Ok(()) => events.extend(closing),
                Err(error) => eprintln!(
                    "horizon-agentd: failed to commit interrupted turn as cancelled for \
                     {session_id:?}: {error}"
                ),
            }
        }

        eprintln!(
            "horizon-agentd: resumed session {session_id:?} ({} event(s))",
            events.len()
        );
        spawn_session_thread(
            state.clone(),
            session_id,
            provider_id,
            role_id,
            workspace_root,
            parent_session_id,
            false,
            restored_worktree,
            events,
        );
    }

    if skipped_terminated > 0 {
        eprintln!(
            "horizon-agentd: skipped resume of {skipped_terminated} already-terminated \
             session(s)"
        );
    }

    if terminated_explorations > 0 {
        eprintln!(
            "horizon-agentd: terminated {terminated_explorations} orphaned exploration \
             session(s) instead of resuming them"
        );
    }
}

/// `docs/agent-explore-design.md` decision 8: an exploration session is
/// meaningless without the `task` call that was folding its
/// events, and that waiter died with the previous process. So a
/// never-completed exploration found in the log is committed as terminated
/// rather than re-adopted -- otherwise it would come back as a live session
/// nothing is listening to, burning a provider budget on a question whose
/// asker is gone.
///
/// No wire field distinguishes these: the explore role id alone identifies
/// them (`roles::is_exploration`), which is why this whole decision cost
/// the session wire nothing.
///
/// The terminal sequence mirrors the interrupted-turn fixup right below its
/// call site: every still-outstanding tool call is closed as cancelled
/// first (nothing survives to answer it), then an explanatory error, then
/// the turn's own end if one was in flight, then `Terminated` -- the state
/// [`session_is_dead`] reads, so a *later* restart skips this session
/// entirely instead of doing this again.
fn terminate_orphaned_exploration(
    writer: &WriterHandle,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
    persisted_context: Option<&PersistedSessionContext>,
    frame: &AgentFrame,
) {
    let mut closing: Vec<Event> = outstanding_tool_call_ids(frame)
        .into_iter()
        .map(|call_id| Event::ToolCallFinished(cancelled_tool_call_result(call_id)))
        .collect();
    closing.push(Event::Error(AgentError {
        message: "Exploration session terminated on daemon restart: the `task` call \
                  waiting on it did not survive."
            .to_string(),
    }));
    if frame.is_turn_in_flight() {
        closing.push(Event::TurnEnded(TurnEndReason::Failed));
    }
    closing.push(Event::StateChanged(SessionState::Terminated));

    let mut appender = Appender::new(
        writer.clone(),
        session_id,
        Some(provider_id.clone()),
        role_id.cloned(),
    );
    if let Some(context) = persisted_context.cloned() {
        appender = appender.with_session_context(context);
    }
    if let Err(error) =
        appender.append_provider_events(closing.into_iter().map(ProviderEvent::from).collect())
    {
        eprintln!(
            "horizon-agentd: failed to record termination of orphaned exploration session \
             {session_id:?}: {error}"
        );
    }
}

/// Whether `frame`'s folded state shows its session already dead: either
/// `SessionState::Terminated` (the state `rig`'s `Command::Shutdown` path
/// sends -- see `providers::rig::session`) or an `Event::Exited` item (the
/// mock provider's shutdown path, `providers::mock`, pairs this with
/// `Terminated`; checked independently here in case a future provider ever
/// sends one without the other). Used by [`resume_persisted_sessions`] to
/// decide which sessions are worth spawning a thread for at all.
pub(super) fn session_is_dead(frame: &AgentFrame) -> bool {
    matches!(frame.state, Some(SessionState::Terminated))
        || frame
            .items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::Exited(_)))
}

/// Every `ToolCallRequested` call id in `frame` that has no matching
/// `ToolCallFinished` yet — i.e. genuinely still outstanding, whether it was
/// waiting on approval, waiting on Horizon to run it, or already running.
/// Used by [`resume_persisted_sessions`] to decide which calls need a
/// synthetic cancelled result when their turn is committed as cancelled.
fn outstanding_tool_call_ids(frame: &AgentFrame) -> Vec<ToolCallId> {
    let mut outstanding = Vec::new();
    for item in &frame.items {
        match item {
            AgentFrameItem::ToolCallRequested(request)
                if !outstanding.contains(&request.call_id) =>
            {
                outstanding.push(request.call_id.clone());
            }
            AgentFrameItem::ToolCallFinished(result) => {
                outstanding.retain(|call_id| call_id != &result.call_id);
            }
            _ => {}
        }
    }
    outstanding
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::judge_test_state;
    use horizon_agent::contract::Exit;
    use std::path::{Path, PathBuf};

    fn open_test_event_log(label: &str) -> (tempfile::TempDir, PathBuf, WriterHandle) {
        let dir = tempfile::tempdir().expect("create event log directory");
        let path = dir.path().join(format!("{label}.jsonl"));
        let (writer, init_rx) = WriterHandle::open(&path);
        match init_rx.recv().expect("writer startup outcome") {
            horizon_agent::persistence::event_log::WriterInit::Ready(_) => {}
            horizon_agent::persistence::event_log::WriterInit::Failed(error) => {
                panic!("writer startup failed: {error}")
            }
        }
        (dir, path, writer)
    }

    fn append_started_turn(
        writer: &WriterHandle,
        session_id: SessionId,
        provider_id: &ProviderId,
        role_id: Option<RoleId>,
    ) {
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(provider_id.clone()),
            role_id,
        );
        appender
            .append_provider_events(
                vec![
                    Event::StateChanged(SessionState::Created),
                    Event::MessageCommitted(horizon_agent::contract::Message {
                        role: horizon_agent::contract::MessageRole::User,
                        text: "find the emit sites".to_string(),
                    }),
                    Event::StateChanged(SessionState::Running),
                ]
                .into_iter()
                .map(ProviderEvent::from)
                .collect(),
            )
            .expect("append a mid-turn session");
    }

    fn persisted_events(path: &Path, session_id: SessionId) -> Vec<Event> {
        horizon_agent::persistence::event_log::read(path)
            .expect("read event log")
            .records
            .into_iter()
            .filter(|record| record.session_id == session_id)
            .map(|record| record.event)
            .collect()
    }

    /// `docs/agent-explore-design.md` decision 8: an exploration session
    /// whose `task` waiter died with the previous daemon process is
    /// committed as terminated rather than resumed -- while an ordinary
    /// session left mid-turn in exactly the same shape still resumes.
    #[test]
    fn daemon_resume_terminates_a_never_completed_exploration_instead_of_adopting_it() {
        let (_dir, path, writer) = open_test_event_log("explore-resume");
        let state = judge_test_state();
        state.set_writer(Some(writer.clone()));

        // The mock provider: a resumed session's thread parks on its command
        // channel without touching a network provider.
        let provider_id = ProviderId("builtin.agent.mock".to_string());
        let explore_id = SessionId::new();
        let ordinary_id = SessionId::new();
        append_started_turn(
            &writer,
            explore_id,
            &provider_id,
            Some(RoleId(horizon_agent::roles::EXPLORE_ROLE_ID.to_string())),
        );
        append_started_turn(&writer, ordinary_id, &provider_id, None);
        writer.flush().expect("flush seeded records");

        let records = horizon_agent::persistence::event_log::read(&path)
            .expect("read seeded event log")
            .records;
        resume_persisted_sessions(&state, records);
        writer.flush().expect("flush resume fixups");

        let live: Vec<SessionId> = state.sessions.lock().unwrap().keys().copied().collect();
        assert!(
            !live.contains(&explore_id),
            "an orphaned exploration session must never be spawned again: {live:?}"
        );
        assert!(
            live.contains(&ordinary_id),
            "an ordinary mid-turn session must still resume: {live:?}"
        );

        let explore_events = persisted_events(&path, explore_id);
        assert!(
            explore_events.contains(&Event::TurnEnded(TurnEndReason::Failed)),
            "the exploration's interrupted turn must be closed: {explore_events:?}"
        );
        assert!(
            explore_events.contains(&Event::StateChanged(SessionState::Terminated)),
            "the exploration must be durably terminated: {explore_events:?}"
        );
        assert!(
            explore_events
                .iter()
                .any(|event| matches!(event, Event::Error(error) if error
                    .message
                    .contains("`task`"))),
            "the termination must say why: {explore_events:?}"
        );

        // A later restart reads that `Terminated` and skips the session
        // entirely, rather than re-terminating it on every boot.
        let records = horizon_agent::persistence::event_log::read(&path)
            .expect("read event log again")
            .records;
        let before = persisted_events(&path, explore_id).len();
        let fresh_state = judge_test_state();
        fresh_state.set_writer(Some(writer.clone()));
        resume_persisted_sessions(&fresh_state, records);
        writer.flush().expect("flush the second resume");
        assert_eq!(
            persisted_events(&path, explore_id).len(),
            before,
            "a second restart must add nothing for an already-terminated exploration"
        );
    }

    /// A session whose log ends in `SessionState::Terminated` (the state
    /// `rig`'s `Command::Shutdown` path sends, with no accompanying
    /// `Event::Exited` -- see `providers::rig::session`) must be treated as
    /// dead.
    #[test]
    fn session_is_dead_when_the_frame_state_is_terminated() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(session_is_dead(&frame));
    }

    /// The mock provider's shutdown path sends `Event::Exited` right after
    /// `SessionState::Terminated`; either one alone must be enough to flag
    /// the session as dead, so this covers `Exited` being present without
    /// relying on the state check.
    #[test]
    fn session_is_dead_when_an_exited_event_is_present() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
            Event::Exited(Exit {
                reason: "shutdown".to_string(),
            }),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(session_is_dead(&frame));
    }

    /// A session parked in an ordinary live state (here, waiting for the
    /// next user message) must not be flagged as dead -- this is the
    /// common case `resume_persisted_sessions` must keep resuming.
    #[test]
    fn session_is_not_dead_when_waiting_for_user() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(!session_is_dead(&frame));
    }

    /// A session with a turn still genuinely in flight (e.g. parked on an
    /// approval, as a `kill -9` mid-turn would leave it) is not dead either
    /// -- `resume_persisted_sessions` handles that case by committing the
    /// interrupted turn as cancelled, not by refusing to resume it.
    #[test]
    fn session_is_not_dead_when_a_turn_is_in_flight() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::WaitingForApproval),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(!session_is_dead(&frame));
    }
}
