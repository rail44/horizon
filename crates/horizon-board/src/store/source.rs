//! Where a store's envelopes come from, and where its writes go.
//!
//! This is the one place that knows which sources a target has: the
//! file-plus-daemon source needs a filesystem and a socket, so on `wasm`
//! only the in-memory source exists. Everything above this file works on
//! the report a source returns, not on how it was obtained.

use std::path::Path;
use std::sync::Arc;

use crate::event::{Envelope, ReadReport};
use crate::store::{memory, types::StoreError};
use crate::wire::{IngestReply, IngestRequest};

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) enum Source {
    /// The project's `events.jsonl` for reads, `horizon-logd` for writes.
    #[cfg(not(target_family = "wasm"))]
    Log(Arc<LogSource>),
    /// A fixed list of envelopes. Reads fold them; writes are refused.
    Memory(Arc<Vec<Envelope>>),
}

/// The two paths the log-backed source is addressed by: the event file it
/// folds, and the daemon socket it sends writes to.
#[cfg(not(target_family = "wasm"))]
pub(crate) struct LogSource {
    pub(crate) events: PathBuf,
    pub(crate) socket: PathBuf,
}

impl Source {
    /// Reads the current envelopes.
    pub(crate) fn report(&self) -> Result<ReadReport, StoreError> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Log(log) => crate::store::file::read_locked(&log.events),
            Self::Memory(envelopes) => Ok(memory::report(envelopes)),
        }
    }

    /// Performs one write operation.
    #[cfg_attr(target_family = "wasm", allow(unused_variables))]
    pub(crate) async fn ingest(&self, request: IngestRequest) -> Result<IngestReply, StoreError> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Log(log) => crate::store::logd::ingest(log, request).await,
            Self::Memory(_) => Err(StoreError::ReadOnly),
        }
    }

    /// The event file this source reads. An in-memory source has no file
    /// and reports an empty path.
    pub(crate) fn path(&self) -> &Path {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Log(log) => &log.events,
            Self::Memory(_) => Path::new(""),
        }
    }
}
