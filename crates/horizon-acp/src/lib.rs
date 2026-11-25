//! What Horizon's agent and shell agree on inside standard ACP v2
//! (`docs/agent-stdio-child-design.md`): the ids of the session config
//! options and their values, and of the permission options. Nothing here
//! extends the protocol.

mod options;

pub use options::*;

pub use horizon_wire::SessionId;
