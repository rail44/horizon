//! Durable input acceptance, outbound delivery and acknowledgement for a live session.

use horizon_agent::contract::{Event, SessionId, SessionInput, pending_input_outcomes};
use horizon_agent::live::LiveState;

use horizon_agent::persistence::event_log::Settled;

use super::events::persist_and_send_session_event;
use super::state::{AgentdState, lock_unpoisoned};

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
    checkpoint_a_settled_position(state, live_state, session_id);
    persist_and_send_session_event(
        state,
        live_state,
        session_id,
        Event::InputAccepted(input.clone()),
    )
}

/// Records the session's files (`crate::checkpoint`) when an input or a
/// user message is about to leave a settled position, keyed by the record
/// the session's history ends with. A fork from the cursor before that
/// message finds it among the records of the same settled stretch.
pub(super) fn checkpoint_a_settled_position(
    state: &AgentdState,
    live_state: &LiveState,
    session_id: SessionId,
) {
    if !state.checkpoints() {
        return;
    }
    let mut settled = Settled::new();
    for event in &live_state.events() {
        settled.apply(event);
    }
    if !settled.is_settled() {
        return;
    }
    let Some(tip) = live_state.tip() else {
        return;
    };
    let dir = lock_unpoisoned(&state.sessions)
        .get(&session_id)
        .and_then(|entry| {
            (entry
                .worktree
                .as_ref()
                .map(|worktree| worktree.path.clone()))
            .or_else(|| entry.workspace_root.clone())
        });
    let Some(dir) = dir else {
        return;
    };
    if let Err(error) = crate::checkpoint::record(&dir, session_id.as_uuid(), &tip) {
        eprintln!("horizon-agentd: no checkpoint for {session_id:?} at {tip}: {error}");
    }
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
