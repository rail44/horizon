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
}
