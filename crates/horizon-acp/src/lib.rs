//! Horizon's extension vocabulary on the ACP line between the shell and
//! `horizon-agentd` (`docs/acp-agentd-design.md`): the `_horizon/*` request
//! and notification types, the `_meta.horizon` payloads carried on standard
//! ACP messages, and the extension version both ends compare.

mod ext;
mod meta;

pub use ext::*;
pub use meta::*;

pub use horizon_wire::SessionId;

/// The extension version both ends compare in `initialize`'s
/// `_meta.horizon.ext_version` ([`InitializeMeta::ext_version`]).
pub const HORIZON_ACP_EXT_VERSION: u32 = 1;
