//! The GPUI shell's agent model and pane view. Shared sessiond transport
//! ownership lives in `crate::sessiond`.

mod follow;
mod session;
mod turns;
mod view;

pub use session::AgentSession;
pub use view::AgentView;
