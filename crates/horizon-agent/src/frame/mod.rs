//! The agent session's UI-facing frame: the data types
//! ([`AgentFrame`]/[`AgentFrameItem`]), the event→frame fold
//! ([`apply_agent_event_to_frame`] and its helpers), and the read-only
//! query functions ([`pending_approval_call_ids_in`] et al.).
//!
//! Split from the former monolithic `frame.rs` by responsibility — see
//! the sibling modules for each concern. The re-exports below preserve
//! the original public surface so no caller changes are needed.

mod fold;
mod queries;
mod types;

#[cfg(test)]
mod test_support;

// --- Re-exports (original `frame.rs` public surface) ---

pub use types::{AgentFrame, AgentFrameItem};

pub use fold::agent_frame_from_events;
pub(crate) use fold::TurnClock;
pub(crate) use fold::{
    agent_frame_and_turn_clock_from_events, apply_agent_event_to_frame,
    apply_tool_call_progress_to_frame,
};

pub(crate) use queries::pending_approval_call_ids_in;
pub use queries::{
    actionable_pending_approval_call_ids_in, halted_awaiting_continue,
    state_indicates_turn_in_flight,
};

#[cfg(test)]
pub(crate) use test_support::{render_agent_transcript, StateEntry};
