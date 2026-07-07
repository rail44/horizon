use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub(crate) const UI_PROFILE_LOG_SCHEMA: &str = "horizon.ui_profile.event_log";
pub(crate) const UI_PROFILE_LOG_VERSION: u32 = 1;

/// One captured UI-thread event: how long Horizon's own handler took to
/// process one window-level event observed by `app::view::app_view`'s
/// global `on_event` chain (see `super`'s module doc for why this is the
/// granularity available -- floem surfaces no real paint/redraw hook).
/// `trigger` is the `floem::event::EventListener` variant name that fired
/// it (e.g. `"KeyDown"`, `"WindowGotFocus"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProfileRecord {
    pub schema: String,
    pub version: u32,
    pub trigger: String,
    pub duration_us: u64,
    pub created_at_unix_ms: u64,
}

impl ProfileRecord {
    pub(crate) fn new(trigger: &str, duration: Duration, created_at_unix_ms: u64) -> Self {
        Self {
            schema: UI_PROFILE_LOG_SCHEMA.to_string(),
            version: UI_PROFILE_LOG_VERSION,
            trigger: trigger.to_string(),
            duration_us: duration.as_micros() as u64,
            created_at_unix_ms,
        }
    }
}

/// Tolerant read of the last `limit` valid records in `path` -- mirrors
/// `crates/horizon-agent`'s event-log reader's "skip corrupt/torn lines
/// instead of failing the whole read" policy (`crates/horizon-agent/src/
/// persistence/event_log/mod.rs::read`), simplified: no sequence numbers to
/// sort by (the writer is a single background thread appending in order, so
/// file order is already chronological), and only the tail is kept so a
/// long-running profiling session's read cost stays bounded instead of
/// growing with the whole file.
pub(crate) fn read_recent(path: &Path, limit: usize) -> std::io::Result<Vec<ProfileRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = std::fs::File::open(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;

    let records: Vec<ProfileRecord> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<ProfileRecord>(line).ok())
        .filter(|record| {
            record.schema == UI_PROFILE_LOG_SCHEMA && record.version == UI_PROFILE_LOG_VERSION
        })
        .collect();

    let start = records.len().saturating_sub(limit);
    Ok(records[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn missing_file_reads_as_empty() {
        let path =
            std::env::temp_dir().join(format!("horizon-ui-profile-missing-{}", Uuid::new_v4()));
        assert_eq!(read_recent(&path, 10).unwrap(), Vec::new());
    }

    #[test]
    fn skips_corrupt_lines_and_keeps_only_the_tail() {
        let path = std::env::temp_dir().join(format!("horizon-ui-profile-tail-{}", Uuid::new_v4()));
        let record_at = |i: u64| ProfileRecord::new("KeyDown", Duration::from_micros(i), i);
        let lines: Vec<String> = (0..5)
            .map(|i| serde_json::to_string(&record_at(i)).unwrap())
            .collect();
        let contents = format!(
            "{}\nnot json\n{}\n{}\n{}\n{}",
            lines[0], lines[1], lines[2], lines[3], lines[4]
        );
        std::fs::write(&path, contents).unwrap();

        let all = read_recent(&path, 100).unwrap();
        assert_eq!(
            all.len(),
            5,
            "the corrupt line must be skipped, not fail the whole read"
        );

        let tail = read_recent(&path, 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].duration_us, 3);
        assert_eq!(tail[1].duration_us, 4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ignores_records_from_a_different_schema_or_version() {
        let path =
            std::env::temp_dir().join(format!("horizon-ui-profile-schema-{}", Uuid::new_v4()));
        let mut wrong_schema = ProfileRecord::new("KeyDown", Duration::from_micros(1), 1);
        wrong_schema.schema = "something.else".to_string();
        let mut wrong_version = ProfileRecord::new("KeyDown", Duration::from_micros(2), 2);
        wrong_version.version = 999;
        let matching = ProfileRecord::new("KeyDown", Duration::from_micros(3), 3);

        let contents = [&wrong_schema, &wrong_version, &matching]
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, contents).unwrap();

        let records = read_recent(&path, 100).unwrap();
        assert_eq!(records, vec![matching]);

        let _ = std::fs::remove_file(&path);
    }
}
