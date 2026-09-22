//! Startup resume: turning the persisted event log back into live session
//! threads, and the fixups a restart owes the sessions it finds there.

mod environment;

use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
use horizon_agent::contract::ProviderId;
use horizon_agent::contract::{
    Error as AgentError, Event, ProviderEvent, SessionId, SessionState, ToolCallId, TurnEndReason,
};
use horizon_agent::frame::{agent_frame_from_events, AgentFrame, AgentFrameItem};
use horizon_agent::persistence::event_log::{Appender, Record};
#[cfg(test)]
use horizon_agent::persistence::event_log::{PersistedSessionContext, WriterHandle};
#[cfg(test)]
use horizon_agent::roles::RoleId;
use horizon_agent::tools::cancelled_tool_call_result;

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
        if role_id.as_ref().is_some_and(|role| retired_role(&role.0)) {
            continue;
        }
        let recorded_events: Vec<_> = session_records
            .iter()
            .map(|record| record.event.clone())
            .collect();
        let frame = agent_frame_from_events(&recorded_events);
        if session_is_dead(&frame) {
            skipped_terminated += 1;
            continue;
        }
        let persisted_context = session_records
            .iter()
            .rev()
            .find_map(|record| record.session_context.clone());
        let restored =
            match environment::restore(session_id, &recorded_events, persisted_context.as_ref()) {
                Ok(restored) => restored,
                Err(error) => {
                    eprintln!("horizon-agentd: {error}");
                    continue;
                }
            };
        let mut events = recorded_events;

        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(provider_id.clone()),
            role_id.clone(),
        )
        .with_turn_history(&session_records);
        if let Some(context) = persisted_context.clone() {
            appender = appender.with_session_context(context);
        }

        if role_id
            .as_ref()
            .is_some_and(horizon_agent::roles::is_exploration)
        {
            terminate_orphaned_exploration(&mut appender, session_id, &frame);
            terminated_explorations += 1;
            continue;
        }

        settle_interrupted_turn(&mut appender, session_id, &frame, &mut events);

        eprintln!(
            "horizon-agentd: resumed session {session_id:?} ({} event(s))",
            events.len()
        );
        super::spawn::spawn_session_thread_with_context(
            state.clone(),
            session_id,
            provider_id,
            role_id,
            restored.workspace_root,
            restored.parent_session_id,
            false,
            restored.worktree,
            events,
            persisted_context,
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

/// Commit interrupted work before a provider receives the restored history.
/// The appender must already carry the original turn history and context.
fn settle_interrupted_turn(
    appender: &mut Appender,
    session_id: SessionId,
    frame: &AgentFrame,
    events: &mut Vec<Event>,
) {
    let outcomes = interrupted_input_outcomes(events);
    if frame.is_turn_in_flight() || !outcomes.is_empty() {
        // Mirrors what a live `Command::Cancel` does (`providers::rig::
        // session`, `providers::mock`): finish every still-outstanding
        // tool call as cancelled *before* the turn-end/state-change
        // pair, so e.g. a call parked in `WaitingForApproval` doesn't
        // keep reading as pending in the resumed frame -- there is no
        // live provider left to eventually answer it.
        let mut closing: Vec<Event> = outstanding_tool_call_ids(frame)
            .into_iter()
            .map(|call_id| Event::ToolCallFinished(cancelled_tool_call_result(call_id)))
            .collect();
        closing.extend(outcomes);
        if frame.is_turn_in_flight() && appender.has_open_turn() {
            closing.push(Event::TurnEnded(TurnEndReason::Cancelled));
        }
        closing.push(Event::StateChanged(SessionState::WaitingForUser));
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
    appender: &mut Appender,
    session_id: SessionId,
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
    if frame.is_turn_in_flight() && appender.has_open_turn() {
        closing.push(Event::TurnEnded(TurnEndReason::Failed));
    }
    closing.push(Event::StateChanged(SessionState::Terminated));

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
        || (frame.state.is_none()
            && frame
                .items
                .iter()
                .any(|item| matches!(item, AgentFrameItem::Exited(_))))
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

/// Explicit owner-triggered restoration of exactly one historical session.
/// Reads acknowledged event-log records, retaining every termination record.
/// Concurrent resume calls serialize through the lifecycle gate.
pub(crate) fn resume_session(
    state: &Arc<AgentdState>,
    session_id: SessionId,
) -> Result<(), String> {
    let mut lifecycle = lock_unpoisoned(&state.lifecycle);
    let writer = state.writer().ok_or("Session persistence is unavailable")?;
    writer.flush().map_err(|error| error.to_string())?;
    let path = lock_unpoisoned(&state.agent_config)
        .persistence
        .event_log_path
        .clone();
    let report =
        horizon_agent::persistence::event_log::read(&path).map_err(|error| error.to_string())?;
    let mut records: Vec<_> = report
        .records
        .into_iter()
        .filter(|record| record.session_id == session_id)
        .collect();
    records.sort_by_key(|record| record.sequence);
    if records.is_empty() {
        if state.session_exists(session_id) {
            return Ok(());
        }
        return Err(format!("No retained history for session {session_id:?}"));
    }
    let recorded_events: Vec<_> = records.iter().map(|record| record.event.clone()).collect();
    if state.session_exists(session_id) {
        if !session_is_dead(&agent_frame_from_events(&recorded_events)) {
            return Ok(());
        }
        // Terminated is persisted before the old thread removes its registry
        // entry. Let cleanup acquire the same lifecycle gate before resuming.
        drop(lifecycle);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_exists(session_id) {
            if std::time::Instant::now() >= deadline {
                return Err("Session termination is still settling".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        lifecycle = lock_unpoisoned(&state.lifecycle);
        if state.session_exists(session_id) {
            return Ok(());
        }
    }
    let _lifecycle = lifecycle;

    let provider_id = records
        .iter()
        .rev()
        .find_map(|record| record.provider_id.clone())
        .ok_or("Retained session has no provider identity")?;
    let role_id = records
        .iter()
        .rev()
        .find_map(|record| record.role_id.clone());
    if role_id
        .as_ref()
        .is_some_and(|role| retired_role(&role.0) || horizon_agent::roles::resolve(role).is_none())
    {
        return Err("Historical role is retired or unavailable".into());
    }
    let context = records
        .iter()
        .rev()
        .find_map(|record| record.session_context.clone())
        .ok_or("Retained session has no environment context")?;
    for grant in &context.filesystem_grants {
        horizon_sandbox::revalidate_grant(grant).map_err(|error| error.to_string())?;
    }
    let mut events: Vec<_> = records.into_iter().map(|record| record.event).collect();
    let identity = events.iter().rev().find_map(|event| match event {
        Event::EnvironmentActivated(identity) => Some(identity),
        _ => None,
    });
    let worktree = if context.isolated_worktree {
        Some(match identity {
            Some(identity) => worktree::restore_worktree(identity, session_id.as_uuid())?,
            None => worktree::adopt_isolated_worktree(
                context
                    .workspace_root
                    .as_deref()
                    .ok_or("Missing retained worktree path")?,
                session_id.as_uuid(),
            )?,
        })
    } else {
        None
    };
    if let Some(root) = &context.workspace_root {
        if !root.is_dir() {
            return Err(format!(
                "Retained consultation directory {} is unavailable",
                root.display()
            ));
        }
    }
    let mut transitions = interrupted_input_outcomes(&events);
    transitions.extend([
        Event::SessionResumed,
        Event::StateChanged(SessionState::Created),
    ]);
    let mut appender = Appender::new(
        writer.clone(),
        session_id,
        Some(provider_id.clone()),
        role_id.clone(),
    )
    .with_session_context(context.clone());
    appender
        .append_provider_events(transitions.iter().cloned().map(Into::into).collect())
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    events.extend(transitions);
    super::spawn::spawn_session_thread_with_context(
        state.clone(),
        session_id,
        provider_id,
        role_id,
        context.workspace_root.clone(),
        context.parent_session_id,
        false,
        worktree,
        events,
        Some(context),
    );
    Ok(())
}

fn retired_role(id: &str) -> bool {
    matches!(
        id,
        "keeper" | "milestone-planner" | "milestone-worker" | "milestone-verifier"
    )
}

/// Settle only inputs that had actually entered work before the process died;
/// accepted requests for other destinations remain queued for restoration.
pub(super) fn interrupted_input_outcomes(events: &[Event]) -> Vec<Event> {
    use horizon_agent::contract::{InputResult, SessionInputOutcome};
    let mut active = Vec::new();
    for event in events {
        match event {
            Event::InputStarted(ids) => active = ids.clone(),
            Event::InputOutcome(outcome) => active.retain(|id| !outcome.input_ids.contains(id)),
            _ => {}
        }
    }
    if active.is_empty() {
        return Vec::new();
    }
    let reply_to = events
        .iter()
        .find_map(|event| match event {
            Event::InputAccepted(input) if input.id == active[0] => Some(input.reply_to.clone()),
            _ => None,
        })
        .flatten();
    vec![Event::InputOutcome(SessionInputOutcome {
        delivery_id: format!("input-result:{}", active[0]),
        input_ids: active,
        reply_to,
        outcome: InputResult::Interrupted,
    })]
}

impl AgentdState {
    /// Acknowledge an outbox record even after its source session ended.
    /// This writes the authoritative log directly; transport records have no
    /// provider-history or transcript projection to mutate in a live session.
    pub(crate) fn acknowledge_delivery(
        &self,
        session_id: SessionId,
        delivery_id: String,
    ) -> Result<(), String> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle);
        let writer = self.writer().ok_or("Session persistence is unavailable")?;
        writer.flush().map_err(|error| error.to_string())?;
        let path = lock_unpoisoned(&self.agent_config)
            .persistence
            .event_log_path
            .clone();
        let report =
            horizon_agent::persistence::event_log::read(path).map_err(|error| error.to_string())?;
        let records: Vec<_> = report
            .records
            .into_iter()
            .filter(|record| record.session_id == session_id)
            .collect();
        if records.iter().any(
            |record| matches!(&record.event, Event::DeliveryAcknowledged(id) if id == &delivery_id),
        ) {
            return Ok(());
        }
        let pending = records.iter().any(|record| match &record.event {
            Event::InputOutcome(outcome) => outcome.delivery_id == delivery_id,
            Event::SessionInputSent { input, .. } => input.id == delivery_id,
            _ => false,
        });
        if !pending {
            return Err("Delivery identity is not in the session outbox".into());
        }
        let record = records.last().ok_or("Session history is unavailable")?;
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            record.provider_id.clone(),
            record.role_id.clone(),
        );
        if let Some(context) = record.session_context.clone() {
            appender = appender.with_session_context(context);
        }
        appender
            .append_provider_events(vec![Event::DeliveryAcknowledged(delivery_id).into()])
            .map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use horizon_agent::contract::{InputResult, SessionInput, SessionInputOutcome};

    #[test]
    fn restart_interrupts_active_destination_and_preserves_other_queued_requests() {
        let input = |id: &str, route: &str| {
            Event::InputAccepted(SessionInput {
                resume_work: false,
                id: id.into(),
                origin: "owner".into(),
                text: id.into(),
                reply_to: Some(route.into()),
            })
        };
        let mut events = vec![
            input("first", "one"),
            input("later", "two"),
            Event::InputStarted(vec!["first".into()]),
        ];
        let outcomes = interrupted_input_outcomes(&events);
        assert!(
            matches!(&outcomes[..], [Event::InputOutcome(SessionInputOutcome { input_ids, reply_to: Some(route), outcome: InputResult::Interrupted, .. })] if input_ids == &["first"] && route == "one")
        );
        events.extend(outcomes);
        assert!(interrupted_input_outcomes(&events).is_empty());
    }

    #[test]
    fn explicit_resume_keeps_same_id_and_retains_prior_termination_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let (writer, ready) = horizon_agent::persistence::event_log::WriterHandle::open(&path);
        assert!(matches!(
            ready.recv().unwrap(),
            horizon_agent::persistence::event_log::WriterInit::Ready(_)
        ));
        let state = crate::session::test_support::judge_test_state();
        state.set_writer(Some(writer.clone()));
        lock_unpoisoned(&state.agent_config)
            .persistence
            .event_log_path = path.clone();
        let session_id = SessionId::new();
        let context = PersistedSessionContext {
            workspace_root: Some(dir.path().to_path_buf()),
            isolated_worktree: false,
            parent_session_id: None,
            filesystem_grants: vec![],
        };
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(ProviderId("builtin.agent.mock".into())),
            None,
        )
        .with_session_context(context);
        let original = Event::MessageCommitted(horizon_agent::contract::Message {
            role: horizon_agent::contract::MessageRole::User,
            text: "retained consultation".into(),
        });
        appender
            .append_provider_events(vec![
                original.clone().into(),
                Event::StateChanged(SessionState::Terminated).into(),
            ])
            .unwrap();
        resume_session(&state, session_id).unwrap();
        resume_session(&state, session_id).unwrap();
        assert!(state.session_exists(session_id));
        let replay = lock_unpoisoned(&state.sessions)
            .get(&session_id)
            .unwrap()
            .replay
            .clone();
        let (reply, receive) = crossbeam_channel::unbounded();
        replay.send(reply).unwrap();
        let events = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(events.contains(&original));
        assert!(events.contains(&Event::StateChanged(SessionState::Terminated)));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::SessionResumed))
                .count(),
            1
        );
        assert!(!session_is_dead(&agent_frame_from_events(&events)));
        state.send_command(session_id, horizon_agent::contract::Command::Shutdown);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_exists(session_id) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn historical_delivery_ack_is_durable_without_resuming_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let (writer, ready) = horizon_agent::persistence::event_log::WriterHandle::open(&path);
        assert!(matches!(
            ready.recv().unwrap(),
            horizon_agent::persistence::event_log::WriterInit::Ready(_)
        ));
        let state = crate::session::test_support::judge_test_state();
        state.set_writer(Some(writer.clone()));
        lock_unpoisoned(&state.agent_config)
            .persistence
            .event_log_path = path.clone();
        let session_id = SessionId::new();
        let mut appender = Appender::new(writer.clone(), session_id, None, None);
        appender
            .append_provider_events(vec![
                Event::InputOutcome(SessionInputOutcome {
                    input_ids: vec!["input".into()],
                    delivery_id: "delivery".into(),
                    reply_to: Some("target".into()),
                    outcome: InputResult::Interrupted,
                })
                .into(),
                Event::StateChanged(SessionState::Terminated).into(),
            ])
            .unwrap();
        state
            .acknowledge_delivery(session_id, "delivery".into())
            .unwrap();
        state
            .acknowledge_delivery(session_id, "delivery".into())
            .unwrap();
        let events: Vec<_> = horizon_agent::persistence::event_log::read(&path)
            .unwrap()
            .records
            .into_iter()
            .map(|record| record.event)
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::DeliveryAcknowledged(_)))
                .count(),
            1
        );
        assert!(!state.session_exists(session_id));
    }
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
        let original_turns: HashMap<_, _> = records
            .iter()
            .filter_map(|record| {
                record
                    .turn_id
                    .clone()
                    .map(|turn_id| (record.session_id, turn_id))
            })
            .collect();
        resume_persisted_sessions(&state, records);
        writer.flush().expect("flush resume fixups");

        for record in horizon_agent::persistence::event_log::read(&path)
            .unwrap()
            .records
        {
            if matches!(record.event, Event::TurnEnded(_)) {
                assert_eq!(
                    record.turn_id.as_ref(),
                    original_turns.get(&record.session_id)
                );
            }
        }

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

    #[test]
    fn startup_refuses_unrestorable_context_before_writing_or_spawning() {
        for missing_root in [false, true] {
            let (_dir, path, writer) = open_test_event_log("unrestorable-context");
            let state = judge_test_state();
            state.set_writer(Some(writer.clone()));
            let session_id = SessionId::new();
            let context = PersistedSessionContext {
                workspace_root: None,
                isolated_worktree: missing_root,
                parent_session_id: None,
                filesystem_grants: if missing_root {
                    vec![]
                } else {
                    vec![horizon_sandbox::FilesystemGrant {
                        path: "relative-authority".into(),
                        access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                        scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
                        excluded_subpaths: vec![],
                    }]
                },
            };
            let mut appender = Appender::new(
                writer.clone(),
                session_id,
                Some(ProviderId("builtin.agent.mock".into())),
                None,
            )
            .with_session_context(context);
            appender
                .append_provider_events(vec![Event::StateChanged(SessionState::Running).into()])
                .unwrap();
            writer.flush().unwrap();
            let records = horizon_agent::persistence::event_log::read(&path)
                .unwrap()
                .records;
            let before = records.clone();

            resume_persisted_sessions(&state, records);
            writer.flush().unwrap();

            assert!(!state.session_exists(session_id));
            assert_eq!(
                horizon_agent::persistence::event_log::read(&path)
                    .unwrap()
                    .records,
                before
            );
        }
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

    #[test]
    fn resume_settles_pending_input_without_inventing_a_turn_end() {
        for finished_turn in [false, true] {
            let (_dir, path, writer) = open_test_event_log("pending-input");
            let state = judge_test_state();
            state.set_writer(Some(writer.clone()));
            let session_id = SessionId::new();
            let mut appender = Appender::new(
                writer.clone(),
                session_id,
                Some(ProviderId("builtin.agent.mock".into())),
                None,
            );
            let mut events = vec![Event::InputStarted(vec!["pending".into()])];
            if finished_turn {
                events.extend([
                    Event::MessageCommitted(horizon_agent::contract::Message {
                        role: horizon_agent::contract::MessageRole::User,
                        text: "hello".into(),
                    }),
                    Event::TurnEnded(TurnEndReason::Completed),
                    Event::StateChanged(SessionState::WaitingForUser),
                ]);
            } else {
                // The process died before the provider committed its first
                // user message, so no persisted turn identity exists yet.
                events.push(Event::StateChanged(SessionState::Running));
            }
            appender
                .append_provider_events(events.into_iter().map(ProviderEvent::from).collect())
                .unwrap();
            writer.flush().unwrap();
            let records = horizon_agent::persistence::event_log::read(&path)
                .unwrap()
                .records;
            resume_persisted_sessions(&state, records);
            writer.flush().unwrap();
            let events = persisted_events(&path, session_id);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Event::TurnEnded(_)))
                    .count(),
                usize::from(finished_turn)
            );
            assert!(interrupted_input_outcomes(&events).is_empty());
            assert!(events.iter().any(|event| matches!(event,
                Event::InputOutcome(outcome) if outcome.input_ids == ["pending"]
            )));
            assert!(!agent_frame_from_events(&events).is_turn_in_flight());
            state.send_command(session_id, horizon_agent::contract::Command::Shutdown);
        }
    }
}
