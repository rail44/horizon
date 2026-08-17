//! The wake policy: pure logic for deciding which board events warrant waking
//! the keeper, filtering self-authored events, coalescing bursts, and tracking
//! the cursor.
//!
//! This module is deliberately pure — no I/O, no async, no sockets. It operates
//! on [`BoardEvent`] values and a [`WakeState`] accumulator, so the decision
//! logic is unit-testable without a running logd. The subscriber task
//! ([`super::task`]) feeds pokes and read-back events into this logic and acts
//! on the resulting [`WakeDecision`].
//!
//! ## v1 policy
//!
//! - **item-created**: always wakes (someone added a new item to the board).
//! - **comment-added**: wakes unless the author is the keeper session itself
//!   (author matches `session:<keeper-session-uuid>`), which prevents the
//!   feedback loop of the keeper waking on its own comments.
//! - **item-updated**: never wakes (status/rank/assignee changes are
//!   owner/integrator decisions, not things the keeper needs to react to).
//!
//! Bursts (multiple events arriving in quick succession) are coalesced into a
//! single wake: the policy accumulates the affected item ids and seq range
//! during a quiet period, then emits one [`WakeDecision::Wake`] when the quiet
//! period elapses. While a wake is in flight (the keeper session is running),
//! further events are accumulated but do not trigger a second wake; they are
//! caught up from the cursor after the keeper finishes.

use horizon_board::{BoardEvent, Envelope};

/// The self-author prefix the keeper uses when commenting. A keeper session's
/// own comments carry `author = "session:<keeper-session-uuid>"` (set by
/// `board.comment`'s dispatch in `horizon_agent::tools::board`), so the wake
/// policy filters any `comment-added` whose author starts with this prefix and
/// matches the active keeper session's id.
///
/// Note: the full author string is `session:<uuid>`, and the uuid is the
/// keeper session's own `SessionId`. The policy compares the full string, not
/// just the prefix, so it only filters *this* keeper session's comments — not
/// comments from a different session that happens to use the same prefix.
const SESSION_AUTHOR_PREFIX: &str = "session:";

/// Whether a single board event should contribute to a wake, given the active
/// keeper session's author string (if a keeper is running or was the last to
/// comment).
///
/// This is the per-event filter — the core of the v1 wake policy. It does not
/// decide *when* to wake (that is the coalescer's job in [`WakeState`]); it
/// only says "this event is interesting" or "this event should be ignored."
fn event_is_wake_relevant(event: &BoardEvent, keeper_author: Option<&str>) -> bool {
    match event {
        BoardEvent::ItemCreated { .. } => true,
        BoardEvent::CommentAdded { author, .. } => {
            // Filter self-authored comments to prevent the feedback loop.
            // `keeper_author` is the full `session:<uuid>` string of the
            // keeper session that is currently running (or most recently ran).
            // If there is no active keeper, we cannot know whether a
            // `session:`-prefixed comment is self-authored, but since no
            // keeper is running there is no loop to prevent — we wake.
            match keeper_author {
                Some(self_author) => author != self_author,
                None => true,
            }
        }
        BoardEvent::ItemUpdated { .. } => false,
    }
}

/// Decodes an author string from a `comment-added` event's author field, if it
/// is a session-authored comment. Returns the raw `session:<uuid>` string
/// (the same format `board.comment`'s dispatch uses). This is used to detect
/// when a comment came from *any* keeper session (not just the currently
/// running one), so the policy can avoid waking on comments from a keeper that
/// has already finished.
///
/// In practice, the v1 policy only filters the *currently running* keeper's
/// author. Comments from a previous keeper session (different uuid) are treated
/// as external writes and do wake — they represent someone (a prior keeper)
/// having added context that a new keeper should be aware of. This function is
/// kept for clarity and future use.
#[allow(dead_code)]
fn session_author(author: &str) -> Option<&str> {
    author.strip_prefix(SESSION_AUTHOR_PREFIX).map(|_| author) // return the full `session:<uuid>` string
}

/// The accumulated state of the wake policy across a stream of pokes and
/// read-back events. Owned by the subscriber task.
///
/// The state machine has two phases:
///
/// 1. **Idle** (`keeper_running: false`): events accumulate in `pending`. When
///    a quiet period elapses with pending events, a [`WakeDecision::Wake`] is
///    emitted and the state transitions to Running.
///
/// 2. **Running** (`keeper_running: true`): events still accumulate in
///    `pending`, but no wake is emitted. When the keeper finishes
///    ([`Self::keeper_finished`]), if there are pending events, a new wake is
///    emitted immediately (the cursor advanced past them); otherwise the state
///    returns to Idle.
#[derive(Debug, Clone)]
pub(crate) struct WakeState {
    /// The last-**delivered** seq — the seq of the last event whose
    /// wake-triggered turn has ended. Persisted across daemon restarts so
    /// the subscriber can re-subscribe from this point and catch up on
    /// anything missed while the daemon was down. Unlike the old
    /// "consumption" cursor, this only advances when a wake's delivery
    /// completes ([`Self::keeper_finished`]), so a restart mid-accumulate
    /// re-reads the undelivered events instead of losing them (board #40).
    cursor: u64,
    /// Whether a keeper session is currently running (wake in flight). While
    /// true, new events accumulate but do not trigger a wake.
    keeper_running: bool,
    /// Item ids and seq range accumulated since the last wake was emitted (or
    /// since the keeper started running). Drained on each [`WakeDecision::Wake`].
    pending_items: Vec<u64>,
    pending_first_seq: Option<u64>,
    pending_last_seq: Option<u64>,
    /// The author string of the most recent keeper session
    /// (`session:<uuid>`), used to filter self-authored comments. Persists
    /// across `keeper_finished` (not cleared) so the last keeper's own
    /// comments are filtered even after its turn ends — closing the None
    /// window (#41 residual). A prior keeper (different uuid) still wakes.
    keeper_author: Option<String>,
    /// The `last_seq` of the most recently emitted wake — the seq the cursor
    /// will advance to when that wake's delivery completes. Set by
    /// [`Self::drain_pending`], consumed by [`Self::keeper_finished`].
    delivered_last_seq: Option<u64>,
}

/// The outcome of feeding events into [`WakeState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WakeDecision {
    /// No wake yet — events were accumulated but the quiet period hasn't
    /// elapsed, or a keeper is already running.
    Pending,
    /// Wake the keeper. Carries the item ids that changed and the seq range
    /// (inclusive) the keeper should examine.
    Wake {
        items: Vec<u64>,
        first_seq: u64,
        last_seq: u64,
    },
}

impl WakeState {
    /// Creates a new state with the given starting cursor (the last-delivered
    /// seq, loaded from persistence on restart) and the last keeper's author
    /// (also from persistence, so the self-author filter survives restart).
    pub(crate) fn new(cursor: u64, keeper_author: Option<String>) -> Self {
        Self {
            cursor,
            keeper_running: false,
            pending_items: Vec::new(),
            pending_first_seq: None,
            pending_last_seq: None,
            keeper_author,
            delivered_last_seq: None,
        }
    }

    /// The current cursor (last-delivered seq). Persist this to disk so a
    /// daemon restart can re-subscribe from here.
    pub(crate) fn cursor(&self) -> u64 {
        self.cursor
    }

    /// The most recent keeper's author string (`session:<uuid>`), if any
    /// keeper has ever run. Persisted alongside the cursor so the
    /// self-author filter survives a restart.
    pub(crate) fn keeper_author(&self) -> Option<&str> {
        self.keeper_author.as_deref()
    }

    /// Records that a keeper session has started. The `keeper_author` is the
    /// full `session:<uuid>` string the keeper will use when commenting, so
    /// self-authored comments can be filtered.
    pub(crate) fn keeper_started(&mut self, keeper_author: String) {
        self.keeper_running = true;
        self.keeper_author = Some(keeper_author);
    }

    /// Records that the keeper session's wake-triggered turn has finished
    /// (delivery completed). Advances the cursor to the delivered wake's
    /// `last_seq` — the events in that range are now confirmed delivered and
    /// will not be re-read on restart. If events accumulated while the keeper
    /// was running, emits a new [`WakeDecision::Wake`] immediately.
    ///
    /// Does **not** clear `keeper_author`: the last keeper's author stays set
    /// so its own comments are filtered even after the turn ends (closing the
    /// None window, #41 residual). A new keeper's `keeper_started`
    /// overwrites it; a prior keeper (different uuid) still wakes.
    pub(crate) fn keeper_finished(&mut self) -> WakeDecision {
        self.keeper_running = false;
        // Advance the cursor to the completed wake's last_seq — delivery is
        // confirmed. This is the only place the cursor advances.
        if let Some(last) = self.delivered_last_seq {
            self.cursor = last;
        }
        if self.pending_items.is_empty() {
            WakeDecision::Pending
        } else {
            self.drain_pending()
        }
    }

    /// Feeds a batch of envelopes (read back from the board event log for seqs
    /// past the cursor) into the policy. Each envelope's seq is its 1-based
    /// line number in the JSONL file.
    ///
    /// Returns `Wake` if the policy decides to wake the keeper now, or
    /// `Pending` if events were accumulated but no wake is emitted yet (a
    /// keeper is running, or the quiet period hasn't elapsed — in the v1
    /// design, the quiet period is zero, so any wake-relevant event that finds
    /// the state idle triggers immediately).
    ///
    /// Envelopes whose seq is at or below the cursor are silently skipped
    /// (already processed). This makes the feed idempotent against re-reads.
    pub(crate) fn feed(&mut self, envelopes: &[(u64, Envelope)]) -> WakeDecision {
        // Track the highest seq examined in this batch — used to extend
        // pending_last_seq to cover the full burst (including trailing
        // non-relevant events). The cursor itself does NOT advance here;
        // it only advances on delivery completion (keeper_finished), so a
        // restart re-reads undelivered events instead of losing them
        // (board #40).
        let mut last_examined = self.cursor;
        for &(seq, ref env) in envelopes {
            // Skip already-delivered events (idempotent re-reads).
            if seq <= self.cursor {
                continue;
            }
            last_examined = seq;

            if event_is_wake_relevant(&env.event, self.keeper_author.as_deref()) {
                self.accumulate(seq, &env.event);
            }
        }

        // The seq range should cover the full burst from the first relevant
        // event to the last event in this batch (even if the last event was
        // non-relevant) — the keeper examines the range, not just the
        // individual relevant events.
        if !self.pending_items.is_empty() {
            self.pending_last_seq = Some(last_examined);
        }

        // v1: no quiet period — if we have pending events and no keeper is
        // running, wake immediately.
        if !self.keeper_running && !self.pending_items.is_empty() {
            self.drain_pending()
        } else {
            WakeDecision::Pending
        }
    }

    /// Accumulates a wake-relevant event into the pending set.
    fn accumulate(&mut self, seq: u64, event: &BoardEvent) {
        let id = match event {
            BoardEvent::ItemCreated { id, .. } | BoardEvent::CommentAdded { id, .. } => *id,
            BoardEvent::ItemUpdated { id, .. } => *id,
        };
        if !self.pending_items.contains(&id) {
            self.pending_items.push(id);
        }
        if self.pending_first_seq.is_none() {
            self.pending_first_seq = Some(seq);
        }
        self.pending_last_seq = Some(seq);
    }

    /// Drains the pending set into a [`WakeDecision::Wake`], clearing it.
    /// Records the wake's `last_seq` in `delivered_last_seq` — the cursor
    /// will advance to it when this wake's delivery completes
    /// ([`Self::keeper_finished`]).
    fn drain_pending(&mut self) -> WakeDecision {
        let first = self.pending_first_seq.unwrap_or(self.cursor);
        let last = self.pending_last_seq.unwrap_or(self.cursor);
        let items = std::mem::take(&mut self.pending_items);
        self.pending_first_seq = None;
        self.pending_last_seq = None;
        self.delivered_last_seq = Some(last);
        WakeDecision::Wake {
            items,
            first_seq: first,
            last_seq: last,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_board::{BoardEvent, Envelope, SCHEMA, VERSION};

    /// Builds an envelope at `at` ms wrapping `event`.
    fn env(at: u64, event: BoardEvent) -> Envelope {
        Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event,
        }
    }

    /// Builds a `(seq, Envelope)` pair for feeding into [`WakeState::feed`].
    fn row(seq: u64, event: BoardEvent) -> (u64, Envelope) {
        (seq, env(seq * 1000, event))
    }

    fn item_created(id: u64) -> BoardEvent {
        BoardEvent::ItemCreated {
            id,
            title: format!("Item {id}"),
            body: String::new(),
            rank: "n".to_string(),
        }
    }

    fn comment(id: u64, author: &str) -> BoardEvent {
        BoardEvent::CommentAdded {
            id,
            author: author.to_string(),
            text: "a comment".to_string(),
        }
    }

    fn item_updated(id: u64) -> BoardEvent {
        BoardEvent::ItemUpdated {
            id,
            status: Some("in-progress".to_string()),
            rank: None,
            assignee: None,
            parent: None,
            depends_on: None,
            links: None,
            title: None,
            body: None,
        }
    }

    // -- author filter tests --------------------------------------------------

    #[test]
    fn item_created_wakes_when_idle() {
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[row(1, item_created(1))]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 1,
            }
        );
    }

    #[test]
    fn comment_from_external_author_wakes() {
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[row(1, comment(1, "owner"))]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 1,
            }
        );
    }

    #[test]
    fn comment_from_keeper_self_is_filtered() {
        let mut state = WakeState::new(0, None);
        let keeper_author = "session:abc-123".to_string();
        state.keeper_started(keeper_author.clone());

        // The keeper comments on item 2 — this should NOT wake.
        let decision = state.feed(&[row(1, comment(2, &keeper_author))]);
        assert_eq!(decision, WakeDecision::Pending);

        // Cursor does not advance — feed tracks consumption for the wake
        // range, but the cursor only advances on delivery completion.
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn comment_from_different_session_wakes_even_when_keeper_running() {
        let mut state = WakeState::new(0, None);
        state.keeper_started("session:keeper-uuid".to_string());

        // A comment from a different session (owner, integrator, or another
        // agent session) should accumulate as pending.
        let decision = state.feed(&[row(1, comment(2, "session:other-uuid"))]);
        // Keeper is running, so no immediate wake — pending.
        assert_eq!(decision, WakeDecision::Pending);

        // When the keeper finishes, the pending event triggers a wake.
        let after_finish = state.keeper_finished();
        assert_eq!(
            after_finish,
            WakeDecision::Wake {
                items: vec![2],
                first_seq: 1,
                last_seq: 1,
            }
        );
    }

    #[test]
    fn item_updated_never_wakes() {
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[row(1, item_updated(1))]);
        assert_eq!(decision, WakeDecision::Pending);
        // Cursor does not advance on read — only on delivery completion.
        assert_eq!(state.cursor(), 0);
    }

    // -- cursor advance tests -------------------------------------------------

    #[test]
    fn non_relevant_event_does_not_block_wake() {
        let mut state = WakeState::new(0, None);
        // item-updated at seq 1 (non-relevant), item-created at seq 2.
        let decision = state.feed(&[row(1, item_updated(1)), row(2, item_created(2))]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![2],
                first_seq: 2,
                last_seq: 2,
            }
        );
        // Cursor does not advance on feed — only on delivery completion.
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn already_processed_seq_is_skipped() {
        let mut state = WakeState::new(3, None);
        // Feeding events at seqs 1-3 (all at or below cursor) should be no-op.
        let decision = state.feed(&[
            row(1, item_created(1)),
            row(2, comment(1, "owner")),
            row(3, item_updated(1)),
        ]);
        assert_eq!(decision, WakeDecision::Pending);
        assert_eq!(state.cursor(), 3);
    }

    #[test]
    fn cursor_advances_on_delivery_not_read() {
        // Feed events — a wake is emitted, but the cursor does NOT advance
        // yet: delivery has not completed.
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[
            row(1, item_created(1)),
            row(2, item_updated(1)),
            row(3, comment(1, "owner")),
            row(4, item_created(2)),
            row(5, item_updated(2)),
        ]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1, 2],
                first_seq: 1,
                last_seq: 5,
            }
        );
        // Cursor stays at 0 — the wake's delivery has not completed.
        assert_eq!(state.cursor(), 0);

        // Delivery completes — cursor advances to the wake's last_seq.
        state.keeper_started("session:keeper".to_string());
        let after = state.keeper_finished();
        assert_eq!(after, WakeDecision::Pending);
        assert_eq!(state.cursor(), 5);
    }

    #[test]
    fn cursor_persists_across_restart() {
        // Full delivery cycle: feed → wake → keeper runs → keeper finishes →
        // cursor advances to 5.
        let mut state = WakeState::new(0, None);
        state.feed(&[
            row(1, item_created(1)),
            row(2, item_updated(1)),
            row(3, comment(1, "owner")),
            row(4, item_created(2)),
            row(5, item_updated(2)),
        ]);
        state.keeper_started("session:keeper".to_string());
        state.keeper_finished();
        let persisted_cursor = state.cursor();
        assert_eq!(persisted_cursor, 5);

        // Restart: new state from the persisted cursor and keeper_author.
        // Events at seqs 1-5 are skipped (already delivered); a new event at
        // seq 6 wakes.
        let mut restarted =
            WakeState::new(persisted_cursor, state.keeper_author().map(String::from));
        let decision = restarted.feed(&[row(6, comment(1, "owner"))]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 6,
                last_seq: 6,
            }
        );
    }

    // -- board #40: cursor does not advance past undelivered events ---------

    /// The core #40 scenario: a wake is triggered, the keeper starts running,
    /// more events accumulate, then the daemon restarts before the keeper's
    /// turn ends. The cursor never advanced, so on restart all events are
    /// re-read and coalesced into ONE wake — no duplicate wakes, no lost
    /// events.
    #[test]
    fn restart_redelivers_undelivered_accumulated_events() {
        let mut state = WakeState::new(0, None);

        // First event triggers a wake. Cursor stays at 0 (delivery incomplete).
        let d1 = state.feed(&[row(1, item_created(1))]);
        assert_eq!(
            d1,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 1,
            }
        );
        assert_eq!(state.cursor(), 0);

        // Keeper starts; more events accumulate while it runs.
        state.keeper_started("session:K1".to_string());
        let d2 = state.feed(&[row(2, item_created(2)), row(3, comment(2, "owner"))]);
        assert_eq!(d2, WakeDecision::Pending);
        assert_eq!(state.cursor(), 0);

        // --- Daemon restarts here ---
        // The cursor was never persisted past 0 (delivery never completed).
        // On restart, the subscriber re-reads from cursor 0 and re-feeds
        // all events. The coalesce logic accumulates them into ONE wake.
        let mut restarted = WakeState::new(0, state.keeper_author().map(String::from));
        let d3 = restarted.feed(&[
            row(1, item_created(1)),
            row(2, item_created(2)),
            row(3, comment(2, "owner")),
        ]);
        // One coalesced wake for all three events — not three separate wakes.
        assert_eq!(
            d3,
            WakeDecision::Wake {
                items: vec![1, 2],
                first_seq: 1,
                last_seq: 3,
            }
        );
        // Cursor still at 0 until this new wake's delivery completes.
        assert_eq!(restarted.cursor(), 0);
    }

    /// The self-author filter survives restart: the persisted keeper_author
    /// filters the last keeper's own past comments during re-read, so no
    /// extra self-wake (the None-window fix, #41 residual).
    #[test]
    fn keeper_author_survives_restart_and_filters_self_comments() {
        let mut state = WakeState::new(0, None);

        // External event triggers a wake.
        state.feed(&[row(1, comment(1, "owner"))]);
        state.keeper_started("session:K1".to_string());

        // K1 comments on item 1 while running — filtered (self-authored).
        let d = state.feed(&[row(2, comment(1, "session:K1"))]);
        assert_eq!(d, WakeDecision::Pending);

        // Keeper finishes; cursor advances to the wake's last_seq (1 — the
        // filtered comment at seq 2 is NOT in the wake's range).
        state.keeper_finished();
        assert_eq!(state.cursor(), 1);

        // keeper_author is NOT cleared — it persists as Some("session:K1").
        assert_eq!(state.keeper_author(), Some("session:K1"));

        // --- Daemon restarts ---
        // The persisted cursor is 1; the persisted keeper_author is
        // "session:K1". On re-read, K1's comment at seq 2 is re-read but
        // filtered (author matches keeper_author). No extra wake.
        let mut restarted = WakeState::new(1, state.keeper_author().map(String::from));
        let d2 = restarted.feed(&[row(2, comment(1, "session:K1"))]);
        assert_eq!(d2, WakeDecision::Pending);
        assert_eq!(restarted.cursor(), 1);
    }

    /// After keeper_finished, keeper_author stays set. A self-comment read
    /// back after the turn ends is filtered — no extra wake (the None-window
    /// fix in the non-restart case).
    #[test]
    fn keeper_author_not_cleared_after_finish_filters_late_self_comment() {
        let mut state = WakeState::new(0, None);
        state.feed(&[row(1, item_created(1))]);
        state.keeper_started("session:K1".to_string());
        state.keeper_finished();
        assert_eq!(state.keeper_author(), Some("session:K1"));

        // A late read-back of K1's own comment is filtered.
        let d = state.feed(&[row(2, comment(1, "session:K1"))]);
        assert_eq!(d, WakeDecision::Pending);
    }

    // -- coalesce tests -------------------------------------------------------

    #[test]
    fn burst_of_events_coalesces_into_one_wake() {
        let mut state = WakeState::new(0, None);
        // Three items created in one batch — should produce one wake with all
        // three item ids and the full seq range.
        let decision = state.feed(&[
            row(1, item_created(1)),
            row(2, item_created(2)),
            row(3, item_created(3)),
        ]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1, 2, 3],
                first_seq: 1,
                last_seq: 3,
            }
        );
    }

    #[test]
    fn mixed_relevant_and_irrelevant_events_coalesce_relevant_only() {
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[
            row(1, item_updated(1)),     // not relevant
            row(2, item_created(2)),     // relevant
            row(3, item_updated(2)),     // not relevant
            row(4, comment(2, "owner")), // relevant
        ]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![2],
                first_seq: 2,
                last_seq: 4,
            }
        );
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn duplicate_item_ids_deduplicated_in_wake() {
        let mut state = WakeState::new(0, None);
        let decision = state.feed(&[
            row(1, comment(1, "owner")),
            row(2, comment(1, "integrator")),
            row(3, item_updated(1)),
        ]);
        assert_eq!(
            decision,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 3,
            }
        );
    }

    // -- multi-wake prevention tests -----------------------------------------

    #[test]
    fn no_wake_while_keeper_running() {
        let mut state = WakeState::new(0, None);
        state.keeper_started("session:keeper".to_string());

        // Events arrive while the keeper is running — accumulate, no wake.
        let decision = state.feed(&[row(1, item_created(1)), row(2, comment(1, "owner"))]);
        assert_eq!(decision, WakeDecision::Pending);

        // Keeper finishes — pending events trigger a single wake.
        let after = state.keeper_finished();
        assert_eq!(
            after,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 2,
            }
        );
    }

    #[test]
    fn keeper_finishes_with_no_pending_returns_to_idle() {
        let mut state = WakeState::new(0, None);
        state.keeper_started("session:keeper".to_string());
        let after = state.keeper_finished();
        assert_eq!(after, WakeDecision::Pending);
        assert!(!state.keeper_running);
    }

    #[test]
    fn wake_after_keeper_finishes_includes_accumulated_range() {
        let mut state = WakeState::new(0, None);
        // First wake (idle).
        let d1 = state.feed(&[row(1, item_created(1))]);
        assert_eq!(
            d1,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 1,
                last_seq: 1,
            }
        );

        // Keeper starts running. More events arrive.
        state.keeper_started("session:keeper".to_string());
        let d2 = state.feed(&[
            row(2, item_created(2)),
            row(3, comment(2, "owner")),
            row(4, item_created(3)),
        ]);
        assert_eq!(d2, WakeDecision::Pending);

        // Keeper finishes — one wake for items 2 and 3, seqs 2-4.
        let d3 = state.keeper_finished();
        assert_eq!(
            d3,
            WakeDecision::Wake {
                items: vec![2, 3],
                first_seq: 2,
                last_seq: 4,
            }
        );

        // State is idle again; a new event wakes immediately.
        let d4 = state.feed(&[row(5, comment(1, "owner"))]);
        assert_eq!(
            d4,
            WakeDecision::Wake {
                items: vec![1],
                first_seq: 5,
                last_seq: 5,
            }
        );
    }

    // -- helper function tests ------------------------------------------------

    #[test]
    fn session_author_extracts_session_prefixed() {
        assert_eq!(session_author("session:abc-123"), Some("session:abc-123"));
        assert_eq!(session_author("owner"), None);
        assert_eq!(session_author("integrator"), None);
    }

    #[test]
    fn event_is_wake_relevant_filters_correctly() {
        let keeper = "session:keeper-uuid";

        // item-created: always relevant.
        assert!(event_is_wake_relevant(&item_created(1), None));
        assert!(event_is_wake_relevant(&item_created(1), Some(keeper)));

        // comment from owner: relevant.
        assert!(event_is_wake_relevant(&comment(1, "owner"), None));
        assert!(event_is_wake_relevant(&comment(1, "owner"), Some(keeper)));

        // comment from keeper itself: filtered (only when keeper_author is set).
        assert!(!event_is_wake_relevant(&comment(1, keeper), Some(keeper)));
        // Without a running keeper, a session: comment is relevant (no loop).
        assert!(event_is_wake_relevant(&comment(1, keeper), None));

        // comment from a different session: relevant.
        assert!(event_is_wake_relevant(
            &comment(1, "session:other"),
            Some(keeper)
        ));

        // item-updated: never relevant.
        assert!(!event_is_wake_relevant(&item_updated(1), None));
        assert!(!event_is_wake_relevant(&item_updated(1), Some(keeper)));
    }
}
