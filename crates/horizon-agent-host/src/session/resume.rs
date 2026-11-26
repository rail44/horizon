//! Startup resume: turning the session logs back into live session
//! threads, and the fixups a restart owes the sessions it finds there.

mod environment;

#[cfg(test)]
use std::collections::HashSet;
use std::sync::Arc;

#[cfg(test)]
use horizon_agent::contract::ProviderId;
use horizon_agent::contract::{
    Error as AgentError, Event, ProviderEvent, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::frame::{AgentFrame, AgentFrameItem, agent_frame_from_events};
#[cfg(test)]
use horizon_agent::persistence::event_log::PersistedSessionContext;
use horizon_agent::persistence::event_log::{
    Appender, Record, WriterHandle, read_branch, read_session, session_ids, session_path,
    session_summary,
};
#[cfg(test)]
use horizon_agent::roles::RoleId;
use horizon_agent::tools::cancelled_tool_call_result;

use super::state::{AgentdState, lock_unpoisoned};
use crate::worktree;

/// `docs/agent-runtime-split-design.md` step 4, "agentd start": for each
/// session log, resumes the session live: any turn still open at that session's
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
///
/// Each session's latest records (`session_summary`) decide whether it is
/// dead without reading the rest of its log; only the others are read, as
/// their branch (reaching into the session a fork was forked from).
pub(crate) fn resume_persisted_sessions(state: &Arc<AgentdState>) {
    let Some(writer) = state.writer() else {
        return;
    };
    let dir = state.sessions_dir();
    let sessions = match session_ids(&dir) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("horizon-agent: listing the session logs failed: {error:#}");
            return;
        }
    };

    let mut tally = ResumeTally::default();
    let mut skipped = Vec::new();
    for session_id in sessions {
        let summary = match session_summary(&session_path(&dir, session_id)) {
            Ok(Some(summary)) => summary,
            Ok(None) => continue,
            Err(error) => {
                eprintln!("horizon-agent: reading session {session_id:?} failed: {error:#}");
                continue;
            }
        };
        if summary.dead {
            // An unavailable role's session never counts as a terminated
            // skip: the per-session body checks the role before liveness.
            if !summary
                .role_id
                .as_ref()
                .is_some_and(|role| unavailable_role(&state.packages(), role))
            {
                tally.skipped_terminated += 1;
            }
            continue;
        }
        match read_session(&dir, session_id) {
            Ok(own) => skipped.extend(own.skipped_summary()),
            Err(error) => {
                eprintln!("horizon-agent: reading session {session_id:?} failed: {error:#}");
                continue;
            }
        }
        let records = match read_branch(&dir, session_id) {
            Ok(records) => records,
            Err(error) => {
                eprintln!("horizon-agent: reading session {session_id:?} failed: {error:#}");
                continue;
            }
        };
        tally.note(resume_one_session(state, &writer, session_id, records));
    }
    if !skipped.is_empty() {
        let summary = skipped.join("; ");
        eprintln!("horizon-agent: {summary} while reading the session logs");
    }
    tally.deliver();
    tally.report();
}

/// What [`resume_one_session`] did with one session; only the counted
/// outcomes feed [`ResumeTally`]'s summary lines.
enum ResumeOutcome {
    /// Retired role, unrestorable environment, or a failed fixup commit.
    NotResumed,
    AlreadyDead,
    ExplorationTerminated(Option<RestartNotice>),
    Spawned,
}

/// What a write child's requester is told after the agent's restart ended the
/// child (`docs/agent-subagent-design.md` decision 11).
struct RestartNotice {
    requester: SessionId,
    child: SessionId,
    description: String,
    /// The branch its work was kept on; `None` when it held none.
    branch: Option<String>,
}

/// Counted rather than printed per session: a real archived log can carry
/// dozens of long-dead sessions, and a line per one drowned out the
/// genuinely interesting "resumed session" lines right next to it.
#[derive(Default)]
struct ResumeTally {
    skipped_terminated: usize,
    // Counted the same way, for the same reason -- see
    // [`terminate_orphaned_exploration`].
    terminated_explorations: usize,
    /// Delivered once every session is resumed, so that each requester is
    /// there to take its notice.
    restart_notices: Vec<RestartNotice>,
}

impl ResumeTally {
    fn note(&mut self, outcome: ResumeOutcome) {
        match outcome {
            ResumeOutcome::AlreadyDead => self.skipped_terminated += 1,
            ResumeOutcome::ExplorationTerminated(notice) => {
                self.terminated_explorations += 1;
                self.restart_notices.extend(notice);
            }
            ResumeOutcome::NotResumed | ResumeOutcome::Spawned => {}
        }
    }

    /// Tells each ended write child's requester where its work is. Called
    /// after every session has been resumed.
    fn deliver(&mut self) {
        for notice in self.restart_notices.drain(..) {
            horizon_agent::tools::deliver_restart_notice(
                notice.requester,
                notice.child,
                notice.description,
                notice.branch,
            );
        }
    }

    fn report(&self) {
        if self.skipped_terminated > 0 {
            eprintln!(
                "horizon-agent: skipped resume of {} already-terminated session(s)",
                self.skipped_terminated
            );
        }

        if self.terminated_explorations > 0 {
            eprintln!(
                "horizon-agent: terminated {} orphaned exploration session(s) instead of \
                 resuming them",
                self.terminated_explorations
            );
        }
    }
}

/// One session's startup resume, shared by both modes: provider and role
/// lookup, the retired-role skip, the fold and [`session_is_dead`],
/// environment restoration, then either the orphaned-exploration
/// termination or the interrupted-turn fixup followed by the spawn.
fn resume_one_session(
    state: &Arc<AgentdState>,
    writer: &WriterHandle,
    session_id: SessionId,
    session_records: Vec<Record>,
) -> ResumeOutcome {
    // A forked session's branch begins in its source: what identifies this
    // session and its environment comes from its own records only.
    let own = || {
        session_records
            .iter()
            .filter(|record| record.session_id == session_id)
    };
    let provider_id = own()
        .rev()
        .find_map(|record| record.provider_id.clone())
        .unwrap_or_else(|| lock_unpoisoned(&state.providers).default_provider_id());
    // Mirrors `provider_id` just above: every record `Appender` writes
    // for a session carries the same `role_id` (see
    // `event_log::Appender::new`), so the last one found scanning from
    // the tail is the session's role for its whole lifetime.
    let role_id = own().rev().find_map(|record| record.role_id.clone());
    if role_id
        .as_ref()
        .is_some_and(|role| unavailable_role(&state.packages(), role))
    {
        return ResumeOutcome::NotResumed;
    }
    let recorded_events: Vec<_> = session_records
        .iter()
        .map(|record| record.event.clone())
        .collect();
    let frame = agent_frame_from_events(&recorded_events);
    if session_is_dead(&frame) {
        return ResumeOutcome::AlreadyDead;
    }
    let persisted_context = own()
        .rev()
        .find_map(|record| record.session_context.clone());
    let own_events: Vec<_> = own().map(|record| record.event.clone()).collect();
    let restored = match environment::restore(session_id, &own_events, persisted_context.as_ref()) {
        Ok(restored) => restored,
        Err(error) => {
            eprintln!("horizon-agent: {error}");
            return ResumeOutcome::NotResumed;
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
        return match terminate_orphaned_exploration(&mut appender, &frame) {
            Ok(()) => ResumeOutcome::ExplorationTerminated(restart_notice(
                session_id,
                role_id.as_ref(),
                &restored,
                &events,
            )),
            Err(error) => {
                eprintln!(
                    "horizon-agent: failed to commit orphaned exploration termination for \
                     {session_id:?}: {error}"
                );
                ResumeOutcome::NotResumed
            }
        };
    }

    if let Err(error) = settle_interrupted_turn(&mut appender, &frame, &mut events) {
        eprintln!(
            "horizon-agent: failed to commit interrupted work for {session_id:?}; \
             skipping resume: {error}"
        );
        return ResumeOutcome::NotResumed;
    }

    eprintln!(
        "horizon-agent: resumed session {session_id:?} ({} event(s))",
        events.len()
    );
    super::spawn::spawn_session_thread_with_context(
        state.clone(),
        session_id,
        provider_id,
        role_id,
        restored.workspace_root,
        restored.spawn_source_session_id,
        restored.worktree,
        events,
        persisted_context,
        appender.tip(),
    );
    ResumeOutcome::Spawned
}
/// Commit interrupted work before a provider receives the restored history.
/// The appender must already carry the original turn history and context.
fn settle_interrupted_turn(
    appender: &mut Appender,
    frame: &AgentFrame,
    events: &mut Vec<Event>,
) -> anyhow::Result<()> {
    let outcomes = interrupted_input_outcomes(events);
    let mut closing = cancel_unfinished_tool_calls(frame);
    let mut restored = events.clone();
    restored.extend(closing.clone());
    for identity in horizon_agent::persistence::event_log::interrupted_conversation_calls(&restored)
        .map_err(anyhow::Error::msg)?
    {
        closing.push(Event::ToolCallFinished(cancelled_tool_call_result(
            identity,
        )));
    }
    if frame.is_turn_in_flight() || !outcomes.is_empty() || !closing.is_empty() {
        // Mirrors what a live `Command::Cancel` does (`providers::rig::
        // session`, `providers::mock`): finish every still-outstanding
        // tool call as cancelled *before* the turn-end/state-change
        // pair, so e.g. a call parked in `WaitingForApproval` doesn't
        // keep reading as pending in the resumed frame -- there is no
        // live provider left to eventually answer it.
        closing.extend(outcomes);
        if frame.is_turn_in_flight() && appender.has_open_turn() {
            closing.push(Event::TurnEnded(TurnEndReason::Cancelled));
        }
        closing.push(Event::StateChanged(SessionState::WaitingForUser));
        appender
            .commit_provider_events(closing.iter().cloned().map(ProviderEvent::from).collect())?;
        events.extend(closing);
    }
    Ok(())
}

/// A write child ended by the restart: its worktree is kept when it holds
/// work and removed when it holds none, and its requester is to be told
/// which (`docs/agent-subagent-design.md` decision 11). `None` for a read
/// child, or one with no recorded requester or worktree.
fn restart_notice(
    child: SessionId,
    role_id: Option<&horizon_agent::roles::RoleId>,
    restored: &environment::RestoredEnvironment,
    events: &[Event],
) -> Option<RestartNotice> {
    if role_id.map(|role| role.0.as_str()) != Some(horizon_agent::roles::WRITE_TASK_ROLE_ID) {
        return None;
    }
    let requester = restored.spawn_source_session_id?;
    let worktree = restored.worktree.as_ref()?;
    let branch = if crate::worktree::holds_work(worktree) {
        Some(worktree.branch.clone())
    } else {
        crate::worktree::remove_worktree_if_clean(worktree);
        None
    };
    // The child's own first message is its task; its label lived only in the
    // requester's call.
    let description = events
        .iter()
        .find_map(|event| match event {
            Event::MessageCommitted(message)
                if message.role == horizon_agent::contract::MessageRole::User =>
            {
                message.text.lines().next().map(|line| {
                    let line: String = line.chars().take(80).collect();
                    line
                })
            }
            _ => None,
        })
        .unwrap_or_else(|| "write task".to_string());
    Some(RestartNotice {
        requester,
        child,
        description,
        branch,
    })
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
    frame: &AgentFrame,
) -> anyhow::Result<()> {
    let mut closing = cancel_unfinished_tool_calls(frame);
    closing.push(Event::Error(AgentError {
        message: "Exploration session terminated when the agent restarted: the `task` call \
                  waiting on it did not survive."
            .to_string(),
    }));
    if frame.is_turn_in_flight() && appender.has_open_turn() {
        closing.push(Event::TurnEnded(TurnEndReason::Failed));
    }
    closing.push(Event::StateChanged(SessionState::Terminated));

    appender.commit_provider_events(closing.into_iter().map(ProviderEvent::from).collect())
}

/// Whether `frame`'s folded state shows its session's thread already ended:
/// `SessionState::Terminated` (the state `rig`'s `Command::Shutdown` path
/// sends -- see `providers::rig::session`), `SessionState::Suspended` (its
/// `Command::Suspend` path; such a session is resumed only on demand, by
/// [`resume_session`]), or an `Event::Exited` item (the
/// mock provider's shutdown path, `providers::mock`, pairs this with
/// `Terminated`; checked independently here in case a future provider ever
/// sends one without the other). Used by [`resume_persisted_sessions`] to
/// decide which sessions are worth spawning a thread for at all.
pub(super) fn session_is_dead(frame: &AgentFrame) -> bool {
    matches!(
        frame.state,
        Some(SessionState::Terminated | SessionState::Suspended)
    ) || (frame.state.is_none()
        && frame
            .items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::Exited(_))))
}

/// Preserve each unfinished attempt's identity when settling durable history.
fn cancel_unfinished_tool_calls(frame: &AgentFrame) -> Vec<Event> {
    frame
        .unfinished_tool_calls()
        .into_iter()
        .map(|request| {
            let result = cancelled_tool_call_result(request.identity());
            Event::ToolCallFinished(result)
        })
        .collect()
}

/// Explicit owner-triggered restoration of exactly one historical session.
/// Reads acknowledged event-log records, retaining every termination record.
/// Concurrent resume calls serialize through the lifecycle gate.
pub fn resume_session(state: &Arc<AgentdState>, session_id: SessionId) -> Result<(), String> {
    let mut lifecycle = lock_unpoisoned(&state.lifecycle);
    let writer = state.writer().ok_or("Session persistence is unavailable")?;
    writer.flush().map_err(|error| error.to_string())?;
    let records = state.flushed_session_records(session_id)?;
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

    let own = || {
        records
            .iter()
            .filter(|record| record.session_id == session_id)
    };
    let provider_id = own()
        .rev()
        .find_map(|record| record.provider_id.clone())
        .ok_or("Retained session has no provider identity")?;
    let role_id = own().rev().find_map(|record| record.role_id.clone());
    if role_id
        .as_ref()
        .is_some_and(|role| unavailable_role(&state.packages(), role))
    {
        return Err("Historical role is retired or unavailable".into());
    }
    let context = own()
        .rev()
        .find_map(|record| record.session_context.clone())
        .ok_or("Retained session has no environment context")?;
    for grant in &context.filesystem_grants {
        horizon_sandbox::revalidate_grant(grant).map_err(|error| error.to_string())?;
    }
    let tip = records.last().map(|record| record.event_id.clone());
    let identity = own().rev().find_map(|record| match &record.event {
        Event::EnvironmentActivated(identity) => Some(identity.clone()),
        _ => None,
    });
    let mut events: Vec<_> = records.into_iter().map(|record| record.event).collect();
    // Same rule as `resume::environment::restore`: an owned worktree is
    // restored from its activation identity, or adopted from the persisted
    // root for records that predate activation.
    let worktree = if context.isolated_worktree {
        Some(match identity {
            Some(identity) => worktree::restore_worktree(&identity, session_id.as_uuid())?,
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
    if let Some(root) = &context.workspace_root
        && !root.is_dir()
    {
        return Err(format!(
            "Retained consultation directory {} is unavailable",
            root.display()
        ));
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
    .with_session_context(context.clone())
    .with_parent(tip);
    appender
        .commit_provider_events(transitions.iter().cloned().map(Into::into).collect())
        .map_err(|error| error.to_string())?;
    events.extend(transitions);
    super::spawn::spawn_session_thread_with_context(
        state.clone(),
        session_id,
        provider_id,
        role_id,
        context.workspace_root.clone(),
        context
            .parent_session_id
            .or(context.spawn_source_session_id),
        worktree,
        events,
        Some(context),
        appender.tip(),
    );
    Ok(())
}

/// A role no longer defined (one retired since the session ran, like the
/// board keeper): its sessions are not resumed, since a provider refuses
/// to start a session on a role it cannot resolve. Built-in and registered
/// roles are composed before the resume task starts (`main.rs`).
fn unavailable_role(
    packages: &horizon_agent::packages::Packages,
    role: &horizon_agent::roles::RoleId,
) -> bool {
    packages.resolve_role(role).is_none()
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
    /// One session's branch (`event_log::read_branch`), for a caller that
    /// has just flushed the writer.
    pub fn flushed_session_records(&self, session_id: SessionId) -> Result<Vec<Record>, String> {
        read_branch(&self.sessions_dir(), session_id).map_err(|error| format!("{error:#}"))
    }

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
        let records = self.flushed_session_records(session_id)?;
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
        )
        .with_parent(Some(record.event_id.clone()));
        if let Some(context) = record.session_context.clone() {
            appender = appender.with_session_context(context);
        }
        appender
            .commit_provider_events(vec![Event::DeliveryAcknowledged(delivery_id).into()])
            .map_err(|error| error.to_string())
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
        std::assert_matches!(
            &outcomes[..], [Event::InputOutcome(SessionInputOutcome { input_ids, reply_to: Some(route), outcome: InputResult::Interrupted, .. })] if input_ids == &["first"] && route == "one"
        );
        events.extend(outcomes);
        assert!(interrupted_input_outcomes(&events).is_empty());
    }

    #[test]
    fn explicit_resume_keeps_same_id_and_retains_prior_termination_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let writer = horizon_agent::persistence::event_log::WriterHandle::open(&path);
        let state = crate::session::test_support::test_state();
        state.set_writer(Some(writer.clone()));
        lock_unpoisoned(&state.agent_config)
            .persistence
            .sessions_dir = path.clone();
        let session_id = SessionId::new();
        let context = PersistedSessionContext {
            workspace_root: Some(dir.path().to_path_buf()),
            isolated_worktree: false,
            parent_session_id: None,
            filesystem_grants: vec![],
            spawn_source_session_id: None,
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
        let (reply, receive) = tokio::sync::oneshot::channel();
        replay.send(reply).unwrap();
        let bootstrap = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), receive)
                    .await
                    .unwrap()
                    .unwrap()
            });
        let events = bootstrap.history;
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
        let writer = horizon_agent::persistence::event_log::WriterHandle::open(&path);
        let state = crate::session::test_support::test_state();
        state.set_writer(Some(writer.clone()));
        lock_unpoisoned(&state.agent_config)
            .persistence
            .sessions_dir = path.clone();
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
        let events: Vec<_> = horizon_agent::persistence::event_log::read_all(&path)
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
    use crate::session::test_support::test_state;
    use horizon_agent::contract::{Exit, ToolCallId};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn restart_closes_each_unfinished_retry_occurrence_in_durable_history() {
        use horizon_agent::contract::{OccurrenceId, ToolCallRequest, ToolCallResult};
        use serde_json::json;

        for first_finished in [true, false] {
            let (_dir, path, writer) = open_test_event_log("interrupted-retry");
            let session_id = SessionId::new();
            let mut appender = Appender::new(writer.clone(), session_id, None, None);
            let request = |id: &str| {
                Event::ToolCallRequested(ToolCallRequest {
                    call_id: ToolCallId("same-call".into()),
                    occurrence_id: OccurrenceId(id.into()),
                    tool_id: "bash".into(),
                    input: json!({"command": "echo retry"}).into(),
                })
            };
            let mut events = vec![request("first"), request("retry")];
            if first_finished {
                events.push(Event::ToolCallFinished(
                    ToolCallResult::new(
                        ToolCallId("same-call".into()),
                        OccurrenceId("first".into()),
                        json!({}),
                    )
                    .superseded_by_retry(&OccurrenceId("retry".into())),
                ));
            }
            events.push(Event::StateChanged(SessionState::ToolRunning));
            appender
                .append_provider_events(events.iter().cloned().map(Into::into).collect())
                .unwrap();
            settle_interrupted_turn(
                &mut appender,
                &agent_frame_from_events(&events),
                &mut events,
            )
            .unwrap();
            writer.flush().unwrap();

            let retained = persisted_events(&path, session_id);
            assert_eq!(retained, events);
            let frame = agent_frame_from_events(&retained);
            let attempts = retained
                .iter()
                .filter(|event| matches!(event, Event::ToolCallRequested(_)))
                .count();
            assert_eq!(attempts, 2);
            assert!(
                frame.unfinished_tool_calls().is_empty(),
                "every attempt must close: {:?}",
                frame.unfinished_tool_calls()
            );
            let first_outcome = retained.iter().find_map(|event| match event {
                Event::ToolCallFinished(result)
                    if result.occurrence_id == OccurrenceId("first".into()) =>
                {
                    Some(&result.outcome)
                }
                _ => None,
            });
            assert_eq!(
                matches!(
                    first_outcome,
                    Some(horizon_agent::contract::ToolOutcome::Superseded { .. })
                ),
                first_finished
            );
            let cancelled: Vec<_> = retained
                .iter()
                .filter_map(|event| match event {
                    Event::ToolCallFinished(result)
                        if result.outcome == horizon_agent::contract::ToolOutcome::Cancelled =>
                    {
                        Some(result.occurrence_id.clone())
                    }
                    _ => None,
                })
                .collect();
            let expected = if first_finished {
                vec!["retry"]
            } else {
                vec!["first", "retry"]
            };
            assert_eq!(
                cancelled,
                expected
                    .into_iter()
                    .map(|id| OccurrenceId(id.into()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn recovery_commits_active_receipts_once_and_preserves_queue_pause() {
        use horizon_agent::contract::{InputResult, SessionInput};
        let (_dir, path, writer) = open_test_event_log("input-recovery");
        let session_id = SessionId::new();
        let mut appender = Appender::new(writer.clone(), session_id, None, None);
        let input = |id: &str| {
            Event::InputAccepted(SessionInput {
                id: id.into(),
                text: id.into(),
                origin: "owner".into(),
                reply_to: Some(id.into()),
                resume_work: false,
            })
        };
        let mut events = vec![
            input("active"),
            Event::InputStarted(vec!["active".into()]),
            input("queued"),
            Event::InputQueuePaused(true),
        ];
        appender
            .append_provider_events(events.iter().cloned().map(Into::into).collect())
            .unwrap();
        settle_interrupted_turn(
            &mut appender,
            &agent_frame_from_events(&events),
            &mut events,
        )
        .unwrap();
        let settled = events.clone();
        // A second recovery of the committed log must not send a second reply.
        settle_interrupted_turn(
            &mut appender,
            &agent_frame_from_events(&events),
            &mut events,
        )
        .unwrap();
        writer.flush().unwrap();
        assert_eq!(events, settled);
        assert_eq!(persisted_events(&path, session_id), settled);
        let receipts = horizon_agent::contract::pending_input_outcomes(&events);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].input_ids, ["active"]);
        assert_eq!(receipts[0].outcome, InputResult::Interrupted);
        assert_eq!(receipts[0].reply_to.as_deref(), Some("active"));
        assert_eq!(
            events.iter().rev().find_map(|event| match event {
                Event::InputQueuePaused(paused) => Some(*paused),
                _ => None,
            }),
            Some(true)
        );
    }

    #[test]
    fn failed_recovery_commit_keeps_the_restored_history_unchanged() {
        let writer = WriterHandle::failed("injected startup failure");
        assert!(writer.flush().is_err());
        let mut appender = Appender::new(writer, SessionId::new(), None, None);
        let original = vec![Event::StateChanged(SessionState::Running)];
        let mut events = original.clone();
        assert!(
            settle_interrupted_turn(
                &mut appender,
                &agent_frame_from_events(&events),
                &mut events
            )
            .is_err()
        );
        assert_eq!(events, original);
    }

    fn open_test_event_log(label: &str) -> (tempfile::TempDir, PathBuf, WriterHandle) {
        let dir = tempfile::tempdir().expect("create event log directory");
        let path = dir.path().join(label);
        let writer = horizon_agent::persistence::event_log::WriterHandle::open(&path);
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
        horizon_agent::persistence::event_log::read_all(path)
            .expect("read event log")
            .records
            .into_iter()
            .filter(|record| record.session_id == session_id)
            .map(|record| record.event)
            .collect()
    }

    /// `docs/agent-explore-design.md` decision 8: an exploration session
    /// whose `task` waiter died with the previous agent process is
    /// committed as terminated rather than resumed -- while an ordinary
    /// session left mid-turn in exactly the same shape still resumes.
    #[test]
    fn daemon_resume_terminates_a_never_completed_exploration_instead_of_adopting_it() {
        let (_dir, path, writer) = open_test_event_log("explore-resume");
        let state = test_state();
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

        let records = horizon_agent::persistence::event_log::read_all(&path)
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
        state.set_sessions_dir(path.clone());
        resume_persisted_sessions(&state);
        writer.flush().expect("flush resume fixups");

        for record in horizon_agent::persistence::event_log::read_all(&path)
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
        let before = persisted_events(&path, explore_id).len();
        let fresh_state = test_state();
        fresh_state.set_writer(Some(writer.clone()));
        fresh_state.set_sessions_dir(path.clone());
        resume_persisted_sessions(&fresh_state);
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
            let state = test_state();
            state.set_writer(Some(writer.clone()));
            let session_id = SessionId::new();
            let context = PersistedSessionContext {
                workspace_root: None,
                isolated_worktree: missing_root,
                parent_session_id: None,
                spawn_source_session_id: None,
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
            let records = horizon_agent::persistence::event_log::read_all(&path)
                .unwrap()
                .records;
            let before = records.clone();

            state.set_sessions_dir(path.clone());
            resume_persisted_sessions(&state);
            writer.flush().unwrap();

            assert!(!state.session_exists(session_id));
            assert_eq!(
                horizon_agent::persistence::event_log::read_all(&path)
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
    /// A suspended session's thread has ended too, so startup leaves it for
    /// an explicit resume (`rig`'s `Command::Suspend` path sends no
    /// `Event::Exited`).
    #[test]
    fn a_suspended_session_is_not_resumed_at_startup() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Suspended),
        ];
        assert!(session_is_dead(&agent_frame_from_events(&events)));
    }

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
            let state = test_state();
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
            state.set_sessions_dir(path.clone());
            resume_persisted_sessions(&state);
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

#[cfg(test)]
mod resume_tests {
    use super::*;
    use horizon_agent::contract::{Message, MessageRole};
    use std::path::Path;
    use std::time::Duration;

    fn mock_provider() -> ProviderId {
        ProviderId("builtin.agent.mock".to_string())
    }

    fn user_message() -> Event {
        Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: "work".to_string(),
        })
    }

    /// Appends each session's events through a plain writer and waits until
    /// its thread has exited, so the next open of the file is the only one.
    fn append_sessions(path: &Path, sessions: &[(SessionId, Option<RoleId>, Vec<Event>)]) {
        let writer = horizon_agent::persistence::event_log::WriterHandle::open(path);
        for (session_id, role_id, events) in sessions {
            let mut appender = Appender::new(
                writer.clone(),
                *session_id,
                Some(mock_provider()),
                role_id.clone(),
            );
            appender
                .append_provider_events(events.iter().cloned().map(ProviderEvent::from).collect())
                .unwrap();
        }
        writer.flush().unwrap();
    }

    fn live_sessions(state: &Arc<AgentdState>) -> HashSet<SessionId> {
        state.sessions.lock().unwrap().keys().copied().collect()
    }

    fn shut_down_sessions(state: &Arc<AgentdState>) {
        for session_id in live_sessions(state) {
            state.send_command(session_id, horizon_agent::contract::Command::Shutdown);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !live_sessions(state).is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Startup reads each session's latest records to tell the sessions its
    /// log left alive from the ended ones, and resumes only the alive.
    #[test]
    fn startup_resumes_the_sessions_their_logs_leave_alive() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let (waiting, mid_turn, ended, suspended) = (
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
        );
        append_sessions(
            &sessions,
            &[
                (
                    waiting,
                    None,
                    vec![Event::StateChanged(SessionState::WaitingForUser)],
                ),
                (
                    mid_turn,
                    None,
                    vec![Event::StateChanged(SessionState::Running), user_message()],
                ),
                (
                    ended,
                    None,
                    vec![
                        Event::StateChanged(SessionState::Running),
                        Event::StateChanged(SessionState::Terminated),
                        user_message(),
                    ],
                ),
                (
                    suspended,
                    None,
                    vec![Event::StateChanged(SessionState::Suspended)],
                ),
            ],
        );
        let state = super::super::test_support::test_state();
        state.set_sessions_dir(sessions.clone());
        state.set_writer(Some(
            horizon_agent::persistence::event_log::WriterHandle::open(&sessions),
        ));

        resume_persisted_sessions(&state);

        assert_eq!(live_sessions(&state), HashSet::from([waiting, mid_turn]));
        shut_down_sessions(&state);
    }
}
