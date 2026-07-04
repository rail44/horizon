mod frames;
mod registry;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) use frames::Frames;
pub(crate) use registry::Registry;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct SessionId(Uuid);

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Round-trips through `Uuid` at the `horizon-agent` crate boundary: the
    /// crate defines its own session id newtype (it can't depend on this
    /// one — see `docs/agent-runtime-split-design.md`), and `agent::mod`'s
    /// `From` impls convert between the two via these two methods.
    pub(crate) fn as_uuid(self) -> Uuid {
        self.0
    }

    pub(crate) fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}
