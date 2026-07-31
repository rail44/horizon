//! **Subscribe to another session's events.** The one seam by which code
//! running for session A observes session B's `contract::Event` stream
//! in-process, without going through the client-facing wire.
//!
//! This is the abstraction `docs/agent-explore-design.md` decision 5 asked
//! for ("the wait is an event subscription... the future common abstraction
//! is *subscribe to another agent session's blocking events (approvals) and
//! stop events*") and `docs/agent-async-task-design.md` decision 6 made
//! load-bearing. Its v1 consumer is the background `task` tool
//! (`horizon_agent::tools::ExplorationHost`, implemented by
//! [`super::exploration::AgentdExplorationHost`]), which uses exactly two
//! classes of event off the stream:
//!
//! - **stop/completion** — `TurnEnded`, `StateChanged(Terminated)`,
//!   `Exited`: what makes a child's report final, queues its notification,
//!   and wakes the requester (`horizon_agent::tools::explore`).
//! - **blocking** — `ApprovalRequested`,
//!   `StateChanged(WaitingForApproval)`: today only used to fail a child
//!   fast, because a v1 child is read-only and can never legitimately
//!   reach one.
//!
//! The second class is why this is a general subscription rather than a
//! completion callback. Write-capable task children
//! (`docs/agent-async-task-design.md` decision 7, explicitly out of scope
//! for v1) need their approvals *forwarded* to the requester rather than
//! failed — and that is one more event kind handled off this same stream,
//! not a new seam. Nothing here filters by kind: a subscriber sees every
//! event the session emits, in order, and decides for itself.
//!
//! **Ordering guarantee.** [`AgentdState::subscribe_to_session`] must be
//! called *before* the observed session's thread is spawned
//! ([`super::spawn::spawn_session_thread`]); the fan-out
//! ([`super::events::send_session_event`]) only reaches subscribers that
//! already exist, so subscribing afterwards can miss the session's own
//! `StateChanged(Created)`.
//!
//! Crossbeam rather than tokio because a subscriber is a plain OS thread,
//! like every session thread here; unbounded so an observed session's send
//! never blocks on a slow observer. A failed send means the subscriber is
//! gone, so its entry is dropped lazily, exactly as the client-facing
//! subscribers are.

use std::collections::HashMap;
use std::sync::Mutex;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::contract::{Event, SessionId};

use super::state::{lock_unpoisoned, AgentdState};

/// Every in-process subscription currently installed, keyed by the
/// *observed* session's id. At most one per session: the only subscriber
/// shape today is "the one requester that launched this child", and a
/// second one would be a second owner of a lifetime that is deliberately
/// single-owner (see `horizon_agent::tools::explore`'s teardown rules).
pub(super) type SessionSubscriptions = Mutex<HashMap<SessionId, Sender<Event>>>;

/// A live subscription to one session's event stream. Held by the
/// subscriber for as long as it cares; released by
/// [`AgentdState::unsubscribe_from_session`] (which the `task` seam calls
/// as part of terminating a child, so a subscription never outlives the
/// session it observes).
pub(super) struct SessionSubscription {
    /// The observed session -- carried so a subscriber never has to
    /// re-thread the id alongside the receiver.
    pub(super) session_id: SessionId,
    pub(super) events: Receiver<Event>,
}

impl AgentdState {
    /// Subscribes to `session_id`'s event stream -- see the module doc,
    /// including the "subscribe before spawning" ordering requirement.
    pub(super) fn subscribe_to_session(&self, session_id: SessionId) -> SessionSubscription {
        let (tx, events) = unbounded();
        lock_unpoisoned(&self.session_subscriptions).insert(session_id, tx);
        SessionSubscription { session_id, events }
    }

    pub(super) fn unsubscribe_from_session(&self, session_id: SessionId) {
        lock_unpoisoned(&self.session_subscriptions).remove(&session_id);
    }

    /// Mirrors one `contract::Event` to `session_id`'s subscriber, if one is
    /// installed. Called from the client-facing fan-out
    /// ([`super::events::send_session_event`]) so both paths see exactly the
    /// same ordered stream. Only `AgentWireEvent::Event` is mirrored: the
    /// other wire events are UI-facing ephemera (progress previews, the
    /// model chip) with nothing a subscriber could fold.
    pub(super) fn publish_to_subscriber(&self, session_id: SessionId, event: &Event) {
        let mut subscriptions = lock_unpoisoned(&self.session_subscriptions);
        if subscriptions
            .get(&session_id)
            .is_some_and(|tx| tx.send(event.clone()).is_err())
        {
            subscriptions.remove(&session_id);
        }
    }

    #[cfg(test)]
    pub(super) fn has_subscriber(&self, session_id: SessionId) -> bool {
        lock_unpoisoned(&self.session_subscriptions).contains_key(&session_id)
    }
}
