use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct PaneId(Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub(crate) struct TabId(Uuid);

impl PaneId {
    pub(in crate::workspace) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl TabId {
    pub(in crate::workspace) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
