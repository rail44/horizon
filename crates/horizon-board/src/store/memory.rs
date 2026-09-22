//! The in-memory source: a fixed list of envelopes, read-only.
//!
//! It exists so a board can be shown where no event file and no daemon are
//! reachable — a `wasm32-wasip2` guest, or a test that wants a board without
//! touching the filesystem. The envelopes go through the same
//! `store::query` code and the same `model::fold` as the file source, so
//! what such a store answers is what the real board logic answers for those
//! events.

use std::sync::Arc;

use crate::event::{self, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};
use crate::store::source::Source;
use crate::store::Store;

/// The timestamp the first envelope from [`sample_envelopes`] carries
/// (2026-01-01T00:00:00Z in unix milliseconds, the unit the event log uses).
const SAMPLE_START_AT: u64 = 1_767_225_600_000;

/// How far apart consecutive sample envelopes are stamped (one minute).
const SAMPLE_STEP_MS: u64 = 60_000;

impl Store {
    /// Opens a read-only store over envelopes held in memory. Reads answer
    /// from them; every write fails with [`StoreError::ReadOnly`] without
    /// reaching `horizon-logd`.
    ///
    /// [`StoreError::ReadOnly`]: crate::StoreError::ReadOnly
    pub fn in_memory(envelopes: Vec<Envelope>) -> Self {
        Self {
            source: Source::Memory(Arc::new(envelopes)),
        }
    }
}

/// Wraps board events as log envelopes, stamping them in order from a fixed
/// start so the same events always produce the same store. The envelopes
/// take their sequence numbers from their position, exactly as lines of the
/// event file do.
pub fn sample_envelopes(events: impl IntoIterator<Item = BoardEvent>) -> Vec<Envelope> {
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at: SAMPLE_START_AT + index as u64 * SAMPLE_STEP_MS,
            event,
        })
        .collect()
}

/// Builds the report a read of these envelopes returns: they are all
/// well-formed by construction, so the only thing to derive is the physical
/// position of each one and the high-water id.
pub(crate) fn report(envelopes: &[Envelope]) -> ReadReport {
    let line_count = envelopes.len() as u64;
    ReadReport {
        envelopes: envelopes.to_vec(),
        sequences: (1..=line_count).collect(),
        max_id: envelopes
            .iter()
            .filter_map(|envelope| event::event_id(&envelope.event))
            .max(),
        corrupt_count: 0,
        skipped_count: 0,
        torn_trailing: false,
        line_count,
    }
}
