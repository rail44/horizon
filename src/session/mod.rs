mod frames;
mod registry;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use frames::Frames;
pub use registry::Registry;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
