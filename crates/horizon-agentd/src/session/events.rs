//! Event fan-out: one session's `AgentWireEvent`s to whichever client
//! attachment is subscribed, and its `contract::Event`s to whichever
//! in-process observer holds a [`super::subscription::SessionSubscription`]
//! on it.

use horizon_agent::contract::{Event, SessionId};
use horizon_agent::live::LiveState;
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

/// The sole publication boundary for newly produced conversation batches.
/// Disabled in-memory stores support coordinator tests; production always logs.
pub(super) fn apply_and_send_session_events(
    state: &AgentdState,
    live: &LiveState,
    session_id: SessionId,
    events: Vec<horizon_agent::contract::ProviderEvent>,
) -> bool {
    match live.extend_provider_events(events.clone()) {
        Ok(_) => {
            for event in events {
                send_session_event(state, session_id, AgentWireEvent::from(&event));
            }
            true
        }
        Err(message) => {
            report_persistence_failure(state, live, session_id, message);
            false
        }
    }
}

pub(super) fn execution_available(
    state: &AgentdState,
    live: &LiveState,
    session_id: SessionId,
) -> bool {
    if let Some(message) = live.persistence_failure() {
        report_persistence_failure(state, live, session_id, message);
        return false;
    }
    true
}

pub(super) fn report_persistence_failure(
    state: &AgentdState,
    live: &LiveState,
    session_id: SessionId,
    message: String,
) {
    if live.mark_persistence_failed(message) {
        for request in live.frame().unfinished_tool_calls() {
            horizon_agent::tools::cancel_tool_execution(session_id, &request.identity());
        }
        for event in live.runtime_failure_events() {
            send_session_event(state, session_id, AgentWireEvent::Event(event));
        }
    }
}

/// Publishes a durable event only after the writer has flushed its queued record.
pub(super) fn persist_and_send_session_event(
    state: &AgentdState,
    live_state: &LiveState,
    session_id: SessionId,
    event: Event,
) -> bool {
    if let Err(message) = live_state.persist_provider_events([event.clone().into()]) {
        report_persistence_failure(state, live_state, session_id, message);
        return false;
    }
    send_session_event(state, session_id, AgentWireEvent::Event(event));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::test_state;
    use crate::session::Connection;
    use horizon_agent::contract::{Event, SessionState};

    #[test]
    fn agent_subscriber_mutex_recovers_after_poisoning() {
        let state = test_state();
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
