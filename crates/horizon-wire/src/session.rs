//! The session identifier — the one piece of a session that is shared
//! vocabulary. `docs/runtime-crate-alignment-design.md` judgment 1: a
//! session is an attachment record, (view kind, runtime, session id), and
//! the id is the only part of it any two runtimes ever need to agree on.
//!
//! The type's doc comment below is reproduced byte-for-byte from where it
//! was defined (`horizon_agent::contract`, which now re-exports it): it is
//! this type's `description` in the committed wire-schema artifact, and the
//! carve-out is required to leave that artifact unchanged.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// This crate's own session identifier: a UUID newtype that serializes as a
/// bare UUID string (serde's transparent treatment of one-field tuple
/// structs) — the shape a future wire/IPC boundary will use (see
/// `docs/agent-runtime-split-design.md`). Horizon has its own shared
/// `session::SessionId` (used across terminal and agent sessions alike) —
/// this crate cannot depend on it (that's the whole point of the split), so
/// the two are distinct types connected by `From` impls at the seam in
/// Horizon's `agent` module.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}
