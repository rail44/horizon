//! The GPUI shell's agent model and pane view. Shared agentd transport
//! ownership lives in `crate::agentd`.

mod session;
mod turns;
mod view;

pub(crate) use session::AgentSession;
pub(crate) use view::AgentView;
