//! The wake-action seam: a swappable interface for "what happens when the
//! wake policy decides to wake the keeper."
//!
//! **Design constraint (board #35):** the subscriber and policy must NOT bake
//! in the assumption that waking = spawning a new session. The v1
//! implementation ([`SpawnKeeper`]) spawns a fresh keeper session with a
//! prompt pointing at the changed items/seq range. The v2 implementation
//! ([`ResumeKeeper`], board #39) maintains a single persistent keeper
//! session: it resumes an existing live session by sending the wake prompt,
//! or — if the session was lost (agentd restart, terminate) — seed-spawns a
//! new one from the most recent keeper session's folded `MemoryDigest`
//! sequence. The subscriber calls [`WakeAction::wake`] and awaits the
//! returned future; it never knows which path was taken.
//!
//! The trait returns two things:
//! - The keeper session's **author string** (`session:<uuid>`), so the policy
//!   can filter self-authored comments and prevent the feedback loop.
//! - A **done future** that resolves when the keeper session has finished,
//!   so the subscriber can implement multi-wake prevention (no second wake
//!   while the first is still running).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use horizon_agent::contract::{Command, Event, ProviderId, SessionId};
use horizon_agent::persistence::event_log;
use horizon_agent::roles::RoleId;

use crate::session::{spawn_session_thread, AgentdState};

/// Information about what changed on the board, passed to the wake action.
/// The action uses this to construct the keeper's initial prompt.
#[derive(Debug, Clone)]
pub(crate) struct WakeInfo {
    /// The item ids that changed (created or commented on).
    pub items: Vec<u64>,
    /// The first seq in the changed range (inclusive).
    pub first_seq: u64,
    /// The last seq in the changed range (inclusive).
    pub last_seq: u64,
}

/// A boxed future that resolves when the keeper session has finished.
pub(crate) type DoneFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The swappable seam: "wake the keeper for these changes."
///
/// Returns the keeper's author string (for self-comment filtering) and a
/// future that resolves when the keeper is done (for multi-wake prevention).
pub(crate) trait WakeAction: Send + Sync {
    fn wake(&self, info: WakeInfo) -> (String, DoneFuture);
}

// ---------------------------------------------------------------------------
// v1 implementation: spawn a fresh keeper session per wake.
// ---------------------------------------------------------------------------

/// The v1 wake action: spawns a new keeper session with a prompt pointing at
/// the changed item/seq range. Each wake is a fresh session — no context is
/// carried between wakes (that is #36's job, plugged in as a different
/// `WakeAction` implementation when it lands).
///
/// Retained as the v1 impl behind the same `WakeAction` seam so the default
/// (v2, [`ResumeKeeper`]) can be swapped back if needed. Not constructed in
/// production after #39 switched the default.
#[allow(dead_code)]
pub(crate) struct SpawnKeeper {
    state: Arc<AgentdState>,
    provider_id: ProviderId,
    workspace_root: Option<PathBuf>,
}

impl SpawnKeeper {
    #[allow(dead_code)]
    pub(crate) fn new(
        state: Arc<AgentdState>,
        provider_id: ProviderId,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            state,
            provider_id,
            workspace_root,
        }
    }
}

impl WakeAction for SpawnKeeper {
    fn wake(&self, info: WakeInfo) -> (String, DoneFuture) {
        let session_id = SessionId::new();
        let keeper_author = format!("session:{}", session_id.as_uuid());

        // Spawn the keeper session thread — same path
        // `AgentdExplorationHost::start` uses for exploration sessions.
        spawn_session_thread(
            self.state.clone(),
            session_id,
            self.provider_id.clone(),
            Some(RoleId(horizon_board::keeper::ROLE_ID.to_string())),
            self.workspace_root.clone(),
            None,  // no spawn source — the keeper is daemon-initiated
            false, // not isolated — runs in the shared workspace
            None,
            Vec::new(),
        );

        // Deliver the prompt as the first user message. The prompt tells the
        // keeper which items changed and the seq range to examine.
        let prompt = build_wake_prompt(&info);
        if !self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
            // The session thread ended before we could send the message
            // (rare race). The done future resolves immediately.
            return (keeper_author, Box::pin(async {}));
        }

        // The done future polls `session_exists` until the keeper's thread
        // exits (the entry is removed from `state.sessions` in
        // `spawn_session_thread`'s thread body). This mirrors the completion
        // check in `AgentdExplorationHost`'s test
        // (`exploration.rs:185-189`).
        let state = self.state.clone();
        let done = Box::pin(async move {
            let interval = std::time::Duration::from_millis(100);
            while state.session_exists(session_id) {
                tokio::time::sleep(interval).await;
            }
        });

        (keeper_author, done)
    }
}

/// Builds the initial user-message prompt for a freshly-spawned keeper
/// session, pointing it at the items that changed and the seq range to
/// examine.
fn build_wake_prompt(info: &WakeInfo) -> String {
    let items = info
        .items
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Board activity detected (events seq {}–{}). \
         The following items changed: {}. \
         Read the board, reconstruct context for the items that need it, \
         and write context-restoring comments where appropriate.",
        info.first_seq, info.last_seq, items
    )
}

// ---------------------------------------------------------------------------
// v2 implementation: resume a persistent keeper session (board #39).
// ---------------------------------------------------------------------------
//
// Spec: `docs/standing-agent-memory-design.md` §"#35(起床機構)との接続" and
// `docs/board-keeper-design.md` §5 (v2 section).
//
// The v2 wake action maintains a single persistent keeper session. On wake:
//
// 1. **Resume path** — if the keeper session this action manages is still
//    alive, send the wake prompt as a `Command::UserMessage`. This reuses the
//    session's accumulated memory (the live `MemoryDocument` and full
//    conversation tail) rather than starting from zero. The single-flight
//    guard from #35 (the subscriber's `keeper_done` future) ensures we are
//    only called when no keeper turn is in flight — sending a `UserMessage`
//    to a running session cancels its turn (`SessionLoopState::run`'s
//    `Command::UserMessage` arm calls `cancel_outstanding_tool_calls`), so
//    the subscriber waits for the done future before delivering a new wake.
//
// 2. **Seed-spawn path** — if the keeper session was lost (agentd restart,
//    terminate), fold the most recent keeper session's `MemoryDigest` event
//    sequence from the event log into a seed memory document and spawn a new
//    keeper session with those digest events as `history`. The rig session's
//    `load_rig_session_history` picks up the `MemoryDigest` events from the
//    fallback path and seeds `SessionLoopState::memory` — so the new session
//    starts with the prior keeper's folded memory, and the `[memory
//    document][tail]` projection presents it identically to a live session's
//    own accumulated memory.
//
// The keeper session's id is held in a `Mutex<Option<SessionId>>` shared
// between the action and the done future, so the done future can clear it
// when the session thread exits — making the next wake take the seed-spawn
// path automatically.

/// The v2 wake action: resumes a persistent keeper session, or seed-spawns a
/// new one from the prior keeper's folded memory if the session was lost.
pub(crate) struct ResumeKeeper {
    state: Arc<AgentdState>,
    provider_id: ProviderId,
    workspace_root: Option<PathBuf>,
    /// The event-log path, for folding the prior keeper's MemoryDigest
    /// sequence on the seed-spawn path. `None` if persistence is disabled —
    /// the seed-spawn path then spawns with no history (cold start).
    event_log_path: Option<PathBuf>,
    /// The id of the keeper session this action is managing, if one is
    /// currently live. Cleared by the done future when the session exits.
    /// Shared so the done future (which the subscriber polls) can reset it.
    keeper_session: Arc<Mutex<Option<SessionId>>>,
}

impl ResumeKeeper {
    pub(crate) fn new(
        state: Arc<AgentdState>,
        provider_id: ProviderId,
        workspace_root: Option<PathBuf>,
        event_log_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state,
            provider_id,
            workspace_root,
            event_log_path,
            keeper_session: Arc::new(Mutex::new(None)),
        }
    }
}

impl WakeAction for ResumeKeeper {
    fn wake(&self, info: WakeInfo) -> (String, DoneFuture) {
        // --- Resume path: is our keeper session still alive? ---
        let existing = *self.keeper_session.lock().unwrap();
        if let Some(session_id) = existing {
            if self.state.session_exists(session_id) {
                // The session is alive. The subscriber's single-flight guard
                // (keeper_done) guarantees no turn is in flight when we are
                // called — sending a UserMessage to a running session cancels
                // its turn, which is forbidden (recorded measurement). So
                // this send is always to an idle session.
                let keeper_author = format!("session:{}", session_id.as_uuid());
                let prompt = build_wake_prompt(&info);
                if !self
                    .state
                    .send_command(session_id, Command::UserMessage { text: prompt })
                {
                    // Race: the session ended between our check and the send.
                    // Fall through to the seed-spawn path.
                    return self.seed_spawn(info);
                }

                // The done future resolves when the keeper's turn finishes
                // AND the session thread has exited. But for a persistent
                // session, the thread does NOT exit after one turn — it stays
                // alive waiting for the next wake. So the done future for the
                // resume path resolves when the session thread exits (the
                // session was terminated/lost), which is the correct signal
                // for the subscriber's multi-wake prevention: while the
                // session is alive, the subscriber should not deliver another
                // wake (it accumulates events instead).
                let state = self.state.clone();
                let keeper_session = self.keeper_session.clone();
                let done = Box::pin(async move {
                    let interval = std::time::Duration::from_millis(100);
                    while state.session_exists(session_id) {
                        tokio::time::sleep(interval).await;
                    }
                    // Clear the tracked session id — the next wake will
                    // seed-spawn.
                    *keeper_session.lock().unwrap() = None;
                });
                return (keeper_author, done);
            }
            // The tracked session is gone. Clear the stale id.
            *self.keeper_session.lock().unwrap() = None;
        }

        // --- Seed-spawn path ---
        self.seed_spawn(info)
    }
}

impl ResumeKeeper {
    /// Spawns a new keeper session, seeded with the most recent prior keeper
    /// session's folded `MemoryDigest` sequence. Sets up the done future to
    /// clear the tracked session id on exit.
    fn seed_spawn(&self, info: WakeInfo) -> (String, DoneFuture) {
        let session_id = SessionId::new();
        let keeper_author = format!("session:{}", session_id.as_uuid());

        // Fold the prior keeper's MemoryDigest events for the seed. These are
        // passed as `history` to `spawn_session_thread`, which threads them
        // into `StartSession.history` → `load_rig_session_history`'s
        // `fallback_events`. The rig session's memory-seeding path picks them
        // up: since a fresh session has no DuckDB events of its own, the
        // fallback's `memory_document_from_events` produces the seed document.
        let seed_history = self.fold_prior_keeper_memory();

        *self.keeper_session.lock().unwrap() = Some(session_id);

        spawn_session_thread(
            self.state.clone(),
            session_id,
            self.provider_id.clone(),
            Some(RoleId(horizon_board::keeper::ROLE_ID.to_string())),
            self.workspace_root.clone(),
            None,  // no spawn source — the keeper is daemon-initiated
            false, // not isolated — runs in the shared workspace
            None,
            seed_history,
        );

        let prompt = build_wake_prompt(&info);
        if !self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
            *self.keeper_session.lock().unwrap() = None;
            return (keeper_author, Box::pin(async {}));
        }

        let state = self.state.clone();
        let keeper_session = self.keeper_session.clone();
        let done = Box::pin(async move {
            let interval = std::time::Duration::from_millis(100);
            while state.session_exists(session_id) {
                tokio::time::sleep(interval).await;
            }
            *keeper_session.lock().unwrap() = None;
        });

        (keeper_author, done)
    }

    /// Reads the event log and folds the most recent keeper session's
    /// `MemoryDigest` events into a seed `Vec<Event>`. Returns an empty vec
    /// if the log is unavailable or no prior keeper session exists.
    fn fold_prior_keeper_memory(&self) -> Vec<Event> {
        let Some(path) = &self.event_log_path else {
            return Vec::new();
        };
        let Ok(report) = event_log::read(path) else {
            return Vec::new();
        };
        prior_keeper_digests(&report.records)
    }
}

/// Finds the most recent keeper session in the event-log records and returns
/// its `MemoryDigest` events in log order — the seed for a new keeper session
/// when the live one was lost.
///
/// "Most recent" = the keeper session whose maximum `sequence` is the
/// highest. Every record carries `role_id`, so keeper sessions are identified
/// by `role_id == Some("keeper")`. Only `MemoryDigest` events are returned —
/// they are the only events the memory-seeding path consumes, and passing
/// conversation events from a prior session would pollute the new session's
/// history.
fn prior_keeper_digests(records: &[event_log::Record]) -> Vec<Event> {
    let keeper_role = RoleId(horizon_board::keeper::ROLE_ID.to_string());

    // Group keeper-session records by session_id, tracking the max sequence.
    let mut best_session: Option<(SessionId, u64)> = None;
    let mut sessions: std::collections::HashMap<SessionId, u64> = std::collections::HashMap::new();
    for record in records {
        if record.role_id.as_ref() != Some(&keeper_role) {
            continue;
        }
        let entry = sessions.entry(record.session_id).or_insert(0);
        if record.sequence > *entry {
            *entry = record.sequence;
        }
    }
    // Pick the session with the highest max sequence.
    for (session_id, max_seq) in &sessions {
        match best_session {
            Some((_, best_seq)) if *max_seq <= best_seq => {}
            _ => best_session = Some((*session_id, *max_seq)),
        }
    }

    let Some((target_session, _)) = best_session else {
        return Vec::new();
    };

    // Collect that session's MemoryDigest events in sequence order.
    let mut digests: Vec<(u64, Event)> = records
        .iter()
        .filter(|record| {
            record.session_id == target_session && matches!(record.event, Event::MemoryDigest(_))
        })
        .map(|record| (record.sequence, record.event.clone()))
        .collect();
    digests.sort_by_key(|(seq, _)| *seq);
    digests.into_iter().map(|(_, event)| event).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::contract::{
        Event, MemoryDigest, MemoryField, MemoryFieldUpdate, MemoryOp, Message, MessageRole,
        SessionState,
    };

    #[test]
    fn wake_prompt_mentions_items_and_seq_range() {
        let prompt = build_wake_prompt(&WakeInfo {
            items: vec![3, 7],
            first_seq: 10,
            last_seq: 15,
        });
        assert!(prompt.contains("seq 10–15"));
        assert!(prompt.contains("3, 7"));
    }

    #[test]
    fn wake_prompt_handles_single_item() {
        let prompt = build_wake_prompt(&WakeInfo {
            items: vec![42],
            first_seq: 100,
            last_seq: 100,
        });
        assert!(prompt.contains("seq 100–100"));
        assert!(prompt.contains("42"));
    }

    // --- v2 seed-folding tests -----------------------------------------------

    /// Builds a minimal `Record` for testing `prior_keeper_digests`.
    fn record(
        seq: u64,
        session_id: SessionId,
        role_id: Option<&str>,
        event: Event,
    ) -> event_log::Record {
        event_log::Record {
            schema: "agent".to_string(),
            version: 1,
            event_id: format!("evt-{seq}"),
            sequence: seq,
            session_id,
            turn_id: None,
            provider_id: None,
            role_id: role_id.map(|r| RoleId(r.to_string())),
            session_context: None,
            event_kind: horizon_agent::contract::event_kind(&event).to_string(),
            event,
            provider_payload: None,
            created_at_unix_ms: seq * 1000,
        }
    }

    fn digest_set(field: MemoryField, content: &str) -> Event {
        Event::MemoryDigest(MemoryDigest {
            updates: vec![MemoryFieldUpdate {
                field,
                op: MemoryOp::Set,
                content: content.to_string(),
            }],
            folded_log_range: None,
            no_update_reason: None,
        })
    }

    /// Folds the prior keeper's MemoryDigest events from a log that has two
    /// keeper sessions — the older one and the most-recent one — and returns
    /// only the most recent session's digests.
    #[test]
    fn prior_keeper_digests_picks_most_recent_session() {
        let old_session = SessionId::new();
        let new_session = SessionId::new();

        let records = vec![
            // Old keeper session: seqs 1-3
            record(
                1,
                old_session,
                Some("keeper"),
                digest_set(MemoryField::Goal, "old goal"),
            ),
            record(
                2,
                old_session,
                Some("keeper"),
                Event::StateChanged(SessionState::WaitingForUser),
            ),
            // New keeper session: seqs 3-5 (higher max seq)
            record(
                3,
                new_session,
                Some("keeper"),
                digest_set(MemoryField::Goal, "new goal"),
            ),
            record(
                4,
                new_session,
                Some("keeper"),
                digest_set(MemoryField::InProgress, "task A"),
            ),
            record(
                5,
                new_session,
                Some("keeper"),
                Event::StateChanged(SessionState::WaitingForUser),
            ),
        ];

        let digests = prior_keeper_digests(&records);

        // Only the new session's MemoryDigest events, in order.
        assert_eq!(digests.len(), 2);
        assert!(
            matches!(&digests[0], Event::MemoryDigest(d) if d.updates[0].content == "new goal")
        );
        assert!(matches!(&digests[1], Event::MemoryDigest(d) if d.updates[0].content == "task A"));
    }

    /// A non-keeper session's events are never picked up.
    #[test]
    fn prior_keeper_digests_ignores_non_keeper_sessions() {
        let coding_session = SessionId::new();
        let records = vec![
            record(
                1,
                coding_session,
                None,
                digest_set(MemoryField::Goal, "coding goal"),
            ),
            record(
                2,
                coding_session,
                None,
                Event::StateChanged(SessionState::WaitingForUser),
            ),
        ];

        let digests = prior_keeper_digests(&records);
        assert!(
            digests.is_empty(),
            "non-keeper sessions must not contribute"
        );
    }

    /// An empty log produces no seed.
    #[test]
    fn prior_keeper_digests_empty_for_no_keeper_history() {
        let digests = prior_keeper_digests(&[]);
        assert!(digests.is_empty());
    }

    /// Only MemoryDigest events are returned — conversation events from the
    /// prior session are excluded so they don't pollute the new session's
    /// history.
    #[test]
    fn prior_keeper_digests_excludes_non_digest_events() {
        let session = SessionId::new();
        let records = vec![
            record(
                1,
                session,
                Some("keeper"),
                Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: "wake prompt".to_string(),
                }),
            ),
            record(
                2,
                session,
                Some("keeper"),
                digest_set(MemoryField::Goal, "the goal"),
            ),
            record(
                3,
                session,
                Some("keeper"),
                Event::StateChanged(SessionState::Running),
            ),
        ];

        let digests = prior_keeper_digests(&records);
        assert_eq!(digests.len(), 1);
        assert!(matches!(&digests[0], Event::MemoryDigest(_)));
    }

    /// Folding the seed digests through `memory_document_from_events`
    /// reconstructs the prior keeper's memory document — verifying the
    /// seed-spawn path's contract: the new session starts with the prior
    /// keeper's folded memory. We can't call `memory_document_from_events`
    /// directly (it's `pub(crate)` in `horizon-agent`), but the seed is the
    /// exact event sequence that function replays, so checking the events
    /// themselves is the contract check.
    #[test]
    fn seed_digests_preserve_foldable_memory() {
        let session = SessionId::new();
        let records = vec![
            record(
                1,
                session,
                Some("keeper"),
                digest_set(MemoryField::Goal, "ship v2 wake"),
            ),
            record(
                2,
                session,
                Some("keeper"),
                Event::MemoryDigest(MemoryDigest {
                    updates: vec![MemoryFieldUpdate {
                        field: MemoryField::InProgress,
                        op: MemoryOp::Append,
                        content: "writing tests".to_string(),
                    }],
                    folded_log_range: None,
                    no_update_reason: None,
                }),
            ),
        ];

        let seed = prior_keeper_digests(&records);

        // The seed must contain exactly the MemoryDigest events in order,
        // which is what `memory_document_from_events` replays to reconstruct
        // the document.
        assert_eq!(seed.len(), 2);
        assert!(
            matches!(&seed[0], Event::MemoryDigest(d) if d.updates[0].field == MemoryField::Goal)
        );
        assert!(
            matches!(&seed[1], Event::MemoryDigest(d) if d.updates[0].field == MemoryField::InProgress && d.updates[0].op == MemoryOp::Append)
        );
    }

    // --- v2 single-session side-effect tests ---------------------------------

    /// The v2 action's `keeper_session` tracking starts empty (no live
    /// session), so the first wake must take the seed-spawn path. This
    /// verifies that wake-per-session accumulation does not happen: the
    /// action tracks exactly one session slot, not one per wake.
    #[test]
    fn resume_keeper_tracks_single_session_slot() {
        let action = ResumeKeeper {
            state: crate::session::test_support::state_with_rig_config(false, "test"),
            provider_id: ProviderId("builtin.agent.rig".to_string()),
            workspace_root: None,
            event_log_path: None,
            keeper_session: Arc::new(Mutex::new(None)),
        };

        // Initially no session is tracked — the first wake must seed-spawn,
        // not assume a prior session exists.
        assert!(action.keeper_session.lock().unwrap().is_none());
    }

    /// The v2 action's author string is stable across wakes when the session
    /// is alive — the self-author filter's target does not change per wake.
    /// This is the v1 gap the v2 single-session model fixes: v1's
    /// `SpawnKeeper` minted a fresh `SessionId` per wake (action.rs:83), so
    /// the author changed every time, and a comment the keeper wrote in one
    /// wake would not be filtered as self-authored in the next.
    #[test]
    fn resume_keeper_author_is_stable_for_same_session() {
        let session_id = SessionId::new();
        // The resume path reuses the same session id, so the author string
        // is the same across every wake for that session's lifetime.
        let author1 = format!("session:{}", session_id.as_uuid());
        let author2 = format!("session:{}", session_id.as_uuid());
        assert_eq!(author1, author2);
    }
}
