//! The domain-free foundation under every Horizon runtime's wire
//! (`docs/runtime-crate-alignment-design.md` judgment 2, implementing
//! `docs/view-runtime-principle.md` principle 3): the codec pin, the
//! per-purpose channel size caps, version negotiation, the socket-path
//! conventions, the session identifier, and the merge-time schema-change
//! classifier.
//!
//! **No session semantics, no domain types, ever.** Nothing here knows what
//! an agent or a terminal is; each runtime's hub trait and vocabulary live
//! with that runtime's own crate. That is what lets `horizon-terminald` and
//! `horizon-agentd` share one handshake, one codec, and one set of caps
//! without either linking the other's graph. (A few size caps are *named*
//! after the channel class they bound — `FRAME_…`, `TERMINAL_EVENT_…` —
//! but they are plain byte counts; no vocabulary crosses this boundary.)

mod channels;
mod codec;
mod negotiate;
pub mod schema_check;
mod session;
pub mod socket;

pub use channels::{
    channel_schema, CappedReceiver, CappedWatchReceiver, COMMAND_MAX_ITEM_BYTES,
    CONTROL_MAX_ITEM_BYTES, FRAME_MAX_ITEM_BYTES, RTC_MAX_REPLY_BYTES, RTC_MAX_REQUEST_BYTES,
    TERMINAL_EVENT_MAX_ITEM_BYTES, TOOL_IO_MAX_ITEM_BYTES,
};
pub use codec::{DecodeSkipLog, WireCodec};
pub use negotiate::{ClientHello, VersionRange};
pub use session::SessionId;
