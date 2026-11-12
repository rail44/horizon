//! The shell's agent-session model: the fold of one attachment's ACP v2
//! session updates, `_horizon/*` notifications, and permission requests
//! into the frame the pane renders, and the structural reading of that
//! frame.

mod fold;
mod queries;
mod transcript;
mod types;

#[cfg(test)]
mod tests;

pub use fold::{AgentEvent, AgentModel};
pub use queries::{actionable_pending_approval_identities_in, state_indicates_turn_in_flight};
pub use transcript::*;
pub use types::*;
