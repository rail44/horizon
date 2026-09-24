//! Standing-role memory and its per-interaction checkpoint.

use crate::tools::MemoryDocument;

#[derive(Default)]
pub(in crate::providers::rig) struct StandingMemory {
    pub(super) document: MemoryDocument,
    pub(super) checkpoint: MemoryCheckpoint,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum MemoryCheckpoint {
    #[default]
    Pending,
    Reminded,
    Satisfied,
}
