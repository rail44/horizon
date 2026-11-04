//! Standing-role memory and its per-interaction checkpoint.
//!
//! The checkpoint asks a standing role to update its memory (or declare that
//! nothing changed) before an interaction's turn completes: it starts
//! `Pending` with each interaction, a successful memory update makes it
//! `Satisfied`, and a turn that completes while it is still `Pending` gets one
//! reminder (`Reminded`) and runs again. Its transitions are the methods
//! below, so that the session loop's files only ask for them.

use crate::contract::MemoryDigest;
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

/// What a completing turn does about the checkpoint
/// ([`StandingMemory::complete_turn`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CheckpointAtTurnEnd {
    /// The memory was updated: the turn completes.
    Satisfied,
    /// The reminder went unanswered: the turn completes with a
    /// `MemoryCheckpointMissed` event.
    Missed,
    /// First miss: remind and run the turn again.
    Remind,
}

impl StandingMemory {
    /// A new interaction: the checkpoint is pending again.
    pub(super) fn begin_interaction(&mut self) {
        self.checkpoint = MemoryCheckpoint::Pending;
    }

    /// A successful memory update (an edit or a declared no-op) applies to
    /// the document and satisfies the checkpoint.
    pub(super) fn record_update(&mut self, digest: &MemoryDigest) {
        self.document.apply(digest);
        self.checkpoint = MemoryCheckpoint::Satisfied;
    }

    /// A turn is completing: whether it may, and the transition to
    /// `Reminded` on the first miss.
    pub(super) fn complete_turn(&mut self) -> CheckpointAtTurnEnd {
        match self.checkpoint {
            MemoryCheckpoint::Satisfied => CheckpointAtTurnEnd::Satisfied,
            MemoryCheckpoint::Reminded => CheckpointAtTurnEnd::Missed,
            MemoryCheckpoint::Pending => {
                self.checkpoint = MemoryCheckpoint::Reminded;
                CheckpointAtTurnEnd::Remind
            }
        }
    }
}
