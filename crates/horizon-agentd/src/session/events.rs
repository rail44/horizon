//! Event fan-out: one session's `AgentWireEvent`s to whichever client
//! attachment is subscribed, and its `contract::Event`s to whichever
//! in-process observer holds a [`super::subscription::SessionSubscription`]
//! on it.

use horizon_agent::contract::SessionId;
use horizon_agent::wire::AgentWireEvent;

use super::state::{lock_unpoisoned, AgentdState};

/// Sends a session-scoped wire event to whichever attachment is currently
/// subscribed to `session_id`, silently dropping it if none is (no client
/// attached right now -- see the module doc; committed events are replayed
/// on the next attach anyway). A failed send means the subscriber's bridge
/// is gone, so its entry is removed rather than kept as a dead letter box.
pub(super) fn send_session_event(
    state: &AgentdState,
    session_id: SessionId,
    event: AgentWireEvent,
) {
    if let AgentWireEvent::Event(event) = &event {
        state.publish_to_subscriber(session_id, event);
    }
    let mut subscribers = lock_unpoisoned(&state.agent_subscribers);
    if subscribers
        .get(&session_id)
        .is_some_and(|tx| tx.send(event).is_err())
    {
        subscribers.remove(&session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::judge_test_state;
    use crate::session::Connection;
    use horizon_agent::contract::{Event, SessionState};

    #[test]
    fn agent_subscriber_mutex_recovers_after_poisoning() {
        let state = judge_test_state();
        let poisoning_state = state.clone();
        let outcome = std::thread::spawn(move || {
            let _subscribers = poisoning_state.agent_subscribers.lock().unwrap();
            panic!("poison subscriber map");
        })
        .join();
        assert!(outcome.is_err());

        let session_id = SessionId::new();
        let mut outgoing_rx = Connection::new(state.clone()).subscribe_agent(session_id);
        let event = Event::StateChanged(SessionState::Running);
        send_session_event(&state, session_id, AgentWireEvent::Event(event.clone()));
        assert_eq!(
            outgoing_rx.try_recv().expect("event after poison recovery"),
            AgentWireEvent::Event(event)
        );
    }
}
