//! The board queries, as pure functions over already-read envelopes.
//!
//! Every `Store` read method is one of these applied to whatever its source
//! returned, so a store backed by a file and a store backed by sample
//! envelopes in memory answer identically for the same events.

use std::collections::{BTreeSet, HashMap};

use crate::event::{BoardEvent, Envelope, ReadReport};
use crate::model::{fold, read_position_advances, sorted_by_rank, Item};
use crate::store::types::ListResult;

/// Lists items by rank, optionally filtered by project-defined state.
/// Without an explicit state filter, closed tasks are hidden unless
/// `include_closed` is true. Returns the full observed state vocabulary.
pub(crate) fn list(
    report: &ReadReport,
    status_filter: Option<&str>,
    include_closed: bool,
) -> ListResult {
    let items = fold(&report.envelopes);
    let sorted = sorted_by_rank(&items);

    let statuses: Vec<String> = {
        let mut s: Vec<String> = sorted
            .iter()
            .map(|i| i.status.clone())
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        s.dedup();
        s
    };

    let items: Vec<Item> = match status_filter {
        Some(filter) => sorted
            .into_iter()
            .filter(|i| i.status == filter)
            .cloned()
            .collect(),
        None => sorted
            .into_iter()
            .filter(|i| include_closed || !i.is_closed)
            .cloned()
            .collect(),
    };

    ListResult {
        items,
        statuses,
        skipped: report.skipped_summary(),
    }
}

/// Returns the full item (with comments) or `None` if the id doesn't exist.
pub(crate) fn show(envelopes: &[Envelope], id: u64) -> Option<Item> {
    fold(envelopes).get(&id).cloned()
}

/// The furthest position `consumer` has been advanced to.
pub(crate) fn cursor(envelopes: &[Envelope], consumer: &str) -> u64 {
    envelopes
        .iter()
        .filter_map(|e| match &e.event {
            BoardEvent::CursorAdvanced {
                consumer: c,
                position,
            } if c == consumer => Some(*position),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Furthest read message per task, ordered by the task's comment sequence.
/// Existing individual read events collapse into one inclusive read prefix.
pub(crate) fn read_positions(envelopes: &[Envelope], reader: &str) -> HashMap<u64, String> {
    let items = fold(envelopes);
    let mut positions: HashMap<u64, String> = HashMap::new();
    for envelope in envelopes {
        if let BoardEvent::ReadAdvanced {
            id,
            reader: owner,
            message_id,
        } = &envelope.event
        {
            if owner == reader
                && items.get(id).is_some_and(|item| {
                    read_position_advances(item, positions.get(id).map(String::as_str), message_id)
                })
            {
                positions.insert(*id, message_id.clone());
            }
        }
    }
    positions
}
