//! The GPUI shell's agent layer: the sessiond connection (shared
//! handshake/wire via `horizon_agent::client`), per-session model
//! entities, and the pane view. See docs/gpui-migration-design.md M4.

mod connection;
mod session;
mod view;

pub use connection::{wait_for_drain, SessiondHandle};
pub use session::AgentSession;
pub use view::AgentView;
