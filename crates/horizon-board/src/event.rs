//! Event types and the tolerant JSONL reader.
//!
//! Three event types — `item-created`, `item-updated`, `comment-added` —
//! are wrapped in a versioned envelope (`schema` + `version` + `at`).
//! The reader follows the agent event log's house style: empty lines are
//! skipped, a torn trailing line (file not ending in `\n`) is dropped, and
//! lines that share our schema/version but carry an unknown event type
//! (a future build's event an old build can't decode) are counted as
//! *skipped* rather than *corrupt* — so the fold never rewinds an id onto
//! a skipped line.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "horizon.board.event_log";
pub(crate) const VERSION: u32 = 1;

/// The versioned envelope persisted as one JSON object per line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: String,
    pub version: u32,
    pub at: u64,
    #[serde(flatten)]
    pub event: BoardEvent,
}

/// The three event types. `item-updated` carries an optional field per
/// updatable attribute; absent fields are `None` (serde's default for
/// `Option`) and serialise as omitted (`skip_serializing_if`).
///
/// `parent` is `Option<Option<u64>>`: outer `None` = unchanged,
/// `Some(None)` = cleared, `Some(Some(id))` = set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BoardEvent {
    #[serde(rename = "item-created")]
    ItemCreated {
        id: u64,
        title: String,
        body: String,
        rank: String,
    },
    #[serde(rename = "item-updated")]
    ItemUpdated {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rank: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<Option<u64>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        depends_on: Option<Vec<u64>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        links: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    #[serde(rename = "comment-added")]
    CommentAdded {
        id: u64,
        author: String,
        text: String,
    },
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
        return DecodedLine::Record(Box::new(env));
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
    pub max_id: Option<u64>,
    pub corrupt_count: u32,
    pub skipped_count: u32,
    pub torn_trailing: bool,
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

/// Reads and tolerantly decodes the event log. Returns an empty report
/// when the file doesn't exist yet (first invocation).
pub fn read(path: &Path) -> std::io::Result<ReadReport> {
    if !path.exists() {
        return Ok(ReadReport::default());
    }
    let text = std::fs::read_to_string(path)?;

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
        match decode_line(line) {
            DecodedLine::Record(env) => {
                if let Some(id) = event_id(&env.event) {
                    report.max_id = Some(report.max_id.map_or(id, |m| m.max(id)));
                }
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

    Ok(report)
}

/// Extracts the item id from any event variant (for max-id tracking).
fn event_id(event: &BoardEvent) -> Option<u64> {
    match event {
        BoardEvent::ItemCreated { id, .. } => Some(*id),
        BoardEvent::ItemUpdated { id, .. } => Some(*id),
        BoardEvent::CommentAdded { id, .. } => Some(*id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_item_created() {
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at: 1000,
            event: BoardEvent::ItemCreated {
                id: 1,
                title: "First".to_string(),
                body: "Body".to_string(),
                rank: "n".to_string(),
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"item-created\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"rank\":\"n\""));
    }

    #[test]
    fn serialize_item_updated_partial() {
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at: 2000,
            event: BoardEvent::ItemUpdated {
                id: 1,
                status: Some("in-progress".to_string()),
                rank: None,
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"item-updated\""));
        assert!(json.contains("\"status\":\"in-progress\""));
        // Absent fields must not appear.
        assert!(!json.contains("\"rank\""));
        assert!(!json.contains("\"assignee\""));
    }

    #[test]
    fn roundtrip_envelope() {
        let env = Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at: 3000,
            event: BoardEvent::CommentAdded {
                id: 2,
                author: "owner".to_string(),
                text: "Looks good".to_string(),
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn read_nonexistent_returns_empty() {
        let report = read(Path::new("/nonexistent/path/events.jsonl")).unwrap();
        assert!(report.envelopes.is_empty());
        assert_eq!(report.corrupt_count, 0);
    }

    #[test]
    fn read_skips_corrupt_and_reports() {
        let dir = std::env::temp_dir().join("horizon-board-test-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        let good = r#"{"schema":"horizon.board.event_log","version":1,"at":1,"type":"item-created","id":1,"title":"A","body":"","rank":"n"}"#;
        let corrupt = "this is not json";
        let good2 = r#"{"schema":"horizon.board.event_log","version":1,"at":2,"type":"item-created","id":2,"title":"B","body":"","rank":"s"}"#;
        std::fs::write(&path, format!("{good}\n{corrupt}\n{good2}\n")).unwrap();

        let report = read(&path).unwrap();
        assert_eq!(report.envelopes.len(), 2);
        assert_eq!(report.corrupt_count, 1);
        assert_eq!(report.max_id, Some(2));
        assert!(report.skipped_summary().is_some());
    }

    #[test]
    fn read_tolerates_future_event_type() {
        let dir = std::env::temp_dir().join("horizon-board-test-future");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        let good = r#"{"schema":"horizon.board.event_log","version":1,"at":1,"type":"item-created","id":1,"title":"A","body":"","rank":"n"}"#;
        let future = r#"{"schema":"horizon.board.event_log","version":1,"at":2,"type":"item-archived","id":1,"reason":"done"}"#;
        std::fs::write(&path, format!("{good}\n{future}\n")).unwrap();

        let report = read(&path).unwrap();
        assert_eq!(report.envelopes.len(), 1);
        assert_eq!(report.skipped_count, 1);
        assert_eq!(report.corrupt_count, 0);
        // The skipped event's id is tracked so we don't reuse it.
        assert_eq!(report.max_id, Some(1));
    }

    #[test]
    fn read_drops_torn_trailing_line() {
        let dir = std::env::temp_dir().join("horizon-board-test-torn");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        let good = r#"{"schema":"horizon.board.event_log","version":1,"at":1,"type":"item-created","id":1,"title":"A","body":"","rank":"n"}"#;
        // No trailing newline → last "line" is torn.
        std::fs::write(&path, format!("{good}\n{{partial")).unwrap();

        let report = read(&path).unwrap();
        assert_eq!(report.envelopes.len(), 1);
        assert!(report.torn_trailing);
    }
}
