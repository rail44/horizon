//! Versioned ordinary task events and their tolerant decoder.
//!
//! Everything here works on text already in memory: the types, the
//! per-line decode, and the report the readers build. Opening the event
//! file (and locking it) is the file source's job — `store::file`.

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "horizon.board.event_log";
pub const VERSION: u32 = 2;

/// The versioned envelope persisted as one JSON object per line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: String,
    pub version: u32,
    pub at: u64,
    #[serde(flatten)]
    pub event: BoardEvent,
}

/// Ordinary task transactions, messages, read positions, and delivery cursors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BoardEvent {
    #[serde(rename = "task-imported")]
    ImportedItem { id: u64, item: crate::Item },
    #[serde(rename = "task-stored")]
    ItemStored { id: u64, item: crate::Item },
    #[serde(rename = "message-added")]
    MessageAdded { id: u64, message: crate::Comment },
    /// Advances the reader through this message, including all earlier task posts.
    #[serde(rename = "read-advanced")]
    ReadAdvanced {
        id: u64,
        reader: String,
        message_id: String,
    },
    #[serde(rename = "cursor-advanced")]
    CursorAdvanced { consumer: String, position: u64 },
    #[serde(rename = "import-high-water")]
    ImportHighWater { id: u64 },
}

/// Header used for the second stage of tolerant decoding — extracts just
/// `schema`/`version`/`at` (and `id` if present) from any line, ignoring
/// the event payload entirely.
#[derive(Deserialize)]
struct EnvelopeHeader {
    schema: String,
    version: u32,
    #[serde(default)]
    id: Option<u64>,
}

enum DecodedLine {
    Record(Box<Envelope>),
    /// Known schema+version but undecodable event (future event type).
    Skipped {
        id: Option<u64>,
    },
    /// Not our format at all, or structurally broken JSON.
    Corrupt,
}

fn decode_line(line: &str) -> DecodedLine {
    if let Ok(env) = serde_json::from_str::<Envelope>(line) {
        if env.schema == SCHEMA && env.version == VERSION {
            return DecodedLine::Record(Box::new(env));
        }
        return DecodedLine::Corrupt;
    }
    // Fall back to header-only decode to distinguish "known schema, unknown
    // event" (skipped) from "corrupt" (not our format / broken JSON).
    match serde_json::from_str::<EnvelopeHeader>(line) {
        Ok(h) if h.schema == SCHEMA && h.version == VERSION => DecodedLine::Skipped { id: h.id },
        _ => DecodedLine::Corrupt,
    }
}

/// The tolerant reader's report: parsed envelopes, a max-id seen across
/// all structurally-valid lines (so new ids never collide with a skipped
/// event's id), and counts of corrupt/skipped lines for reporting.
#[derive(Default)]
pub struct ReadReport {
    pub envelopes: Vec<Envelope>,
    pub sequences: Vec<u64>,
    pub max_id: Option<u64>,
    pub corrupt_count: u32,
    pub skipped_count: u32,
    pub torn_trailing: bool,
    /// The number of non-empty, non-torn lines in the file — the 1-based
    /// sequence number the next appended line will get. Used by the logd
    /// subscribe path (`docs/logd-design.md` Subscription shape): the seq
    /// is the line index in the JSONL, so a consumer that misses the stream
    /// and catches up via `tail -n +N -F` sees byte-identical results.
    pub line_count: u64,
}

impl ReadReport {
    pub fn skipped_summary(&self) -> Option<String> {
        if self.corrupt_count == 0 && !self.torn_trailing && self.skipped_count == 0 {
            return None;
        }
        let mut parts = Vec::new();
        if self.corrupt_count > 0 {
            parts.push(format!(
                "{} corrupt line{}",
                self.corrupt_count,
                if self.corrupt_count == 1 { "" } else { "s" }
            ));
        }
        if self.skipped_count > 0 {
            parts.push(format!(
                "{} line{} with an undecodable event",
                self.skipped_count,
                if self.skipped_count == 1 { "" } else { "s" }
            ));
        }
        if self.torn_trailing {
            parts.push("a torn trailing line".to_string());
        }
        Some(format!("skipped {}", parts.join(" and ")))
    }
}

/// Tolerantly decodes the whole event log held as text.
pub fn read_text(text: &str) -> ReadReport {
    let torn_trailing = !text.is_empty() && !text.ends_with('\n');
    let mut lines: Vec<&str> = text.lines().collect();
    if torn_trailing {
        lines.pop(); // drop the partial line
    }

    let mut report = ReadReport {
        torn_trailing,
        ..ReadReport::default()
    };

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        report.line_count += 1;
        match decode_line(line) {
            DecodedLine::Record(env) => {
                if let Some(id) = event_id(&env.event) {
                    report.max_id = Some(report.max_id.map_or(id, |m| m.max(id)));
                }
                report.sequences.push(report.line_count);
                report.envelopes.push(*env);
            }
            DecodedLine::Skipped { id } => {
                if let Some(id) = id {
                    report.max_id = Some(report.max_id.map_or(id, |m| m.max(id)));
                }
                report.skipped_count += 1;
            }
            DecodedLine::Corrupt => {
                report.corrupt_count += 1;
            }
        }
    }

    report
}

/// Extracts the item id from any event variant (for max-id tracking).
pub(crate) fn event_id(event: &BoardEvent) -> Option<u64> {
    match event {
        BoardEvent::ImportedItem { id, .. }
        | BoardEvent::ItemStored { id, .. }
        | BoardEvent::MessageAdded { id, .. }
        | BoardEvent::ReadAdvanced { id, .. }
        | BoardEvent::ImportHighWater { id } => Some(*id),
        BoardEvent::CursorAdvanced { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_schema_is_not_folded_as_an_active_task() {
        let line=serde_json::json!({"schema":SCHEMA,"version":1,"at":0,"type":"item-created","id":1,"title":"old","body":"","rank":"n"}).to_string();
        assert!(matches!(decode_line(&line), DecodedLine::Corrupt));
    }
}
