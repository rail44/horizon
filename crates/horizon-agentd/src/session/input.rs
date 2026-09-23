//! Durable input acceptance, outbound delivery and acknowledgement for a live session.

use horizon_agent::contract::{pending_input_outcomes, Event, SessionId, SessionInput};
use horizon_agent::live::LiveState;

use super::events::persist_and_send_session_event;
use super::state::AgentdState;

pub(super) fn send_input(
    state: &AgentdState,
    live_state: &LiveState,
    session_id: SessionId,
    recipient: SessionId,
    input: SessionInput,
) {
    if live_state.events().iter().any(
        |event| matches!(event, Event::SessionInputSent { input: sent, .. } if sent.id == input.id),
    ) {
        return;
    }
    persist_and_send_session_event(
        state,
        live_state,
        session_id,
        Event::SessionInputSent {
            session_id: recipient,
            input,
        },
    );
}

/// Returns whether the accepted input may now be forwarded to the provider.
pub(super) fn accept_input(
    state: &AgentdState,
    live_state: &LiveState,
    session_id: SessionId,
    input: &SessionInput,
) -> bool {
    if live_state
        .events()
        .iter()
        .any(|event| matches!(event, Event::InputAccepted(accepted) if accepted.id == input.id))
    {
        return false;
    }
    persist_and_send_session_event(
        state,
        live_state,
        session_id,
        Event::InputAccepted(input.clone()),
    )
}

pub(super) fn acknowledge_delivery(
    state: &AgentdState,
    live_state: &LiveState,
    session_id: SessionId,
    delivery_id: String,
) {
    let events = live_state.events();
    let pending_result = pending_input_outcomes(&events)
        .iter()
        .any(|outcome| outcome.delivery_id == delivery_id);
    let pending_send = events.iter().any(
        |event| matches!(event, Event::SessionInputSent { input, .. } if input.id == delivery_id),
    ) && !events
        .iter()
        .any(|event| matches!(event, Event::DeliveryAcknowledged(id) if id == &delivery_id));
    if pending_result || pending_send {
        persist_and_send_session_event(
            state,
            live_state,
            session_id,
            Event::DeliveryAcknowledged(delivery_id),
        );
    }
}

#[cfg(test)]
mod tests;
