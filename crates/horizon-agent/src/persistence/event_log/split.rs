//! Splitting the single log every session used to share into one file per
//! session (`docs/agent-stdio-child-design.md`, "One log file per
//! session"). Lines are copied as they are: a record's `sequence` from the
//! shared log still increases within its session, which is all a session
//! file needs.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::contract::SessionId;

use super::{AGENT_EVENT_LOG_SCHEMA, AGENT_EVENT_LOG_VERSION, UnsupportedEventLogFormat};

/// What a split did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct SplitReport {
    pub sessions: usize,
    pub lines: usize,
    /// Complete lines that name no session (corrupt), left behind.
    pub dropped_lines: usize,
    /// The log ended in an unterminated line, left behind.
    pub ignored_partial_line: bool,
}

/// Splits the shared log at `legacy` into one file per session in `dir`,
/// which must not exist yet (or be empty): the files are written into a
/// directory beside it, which then takes its name, so a split cut short
/// leaves no partial `dir`. The shared log is left in place. A log of
/// another version is refused with [`UnsupportedEventLogFormat`].
pub fn split_legacy_log(legacy: &Path, dir: &Path) -> Result<SplitReport> {
    let bytes = std::fs::read(legacy)
        .with_context(|| format!("read agent event log {}", legacy.display()))?;
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut report = SplitReport {
        ignored_partial_line: complete_len != bytes.len(),
        ..SplitReport::default()
    };
    let mut sessions: HashMap<SessionId, Vec<u8>> = HashMap::new();
    for line in bytes[..complete_len].split_inclusive(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            report.dropped_lines += 1;
            continue;
        };
        if value.get("schema").and_then(serde_json::Value::as_str) == Some(AGENT_EVENT_LOG_SCHEMA)
            && value.get("version").and_then(serde_json::Value::as_u64)
                != Some(u64::from(AGENT_EVENT_LOG_VERSION))
        {
            return Err(UnsupportedEventLogFormat {
                path: legacy.to_path_buf(),
                found: value.get("version").and_then(serde_json::Value::as_u64),
            }
            .into());
        }
        let Some(session_id) = value
            .get("session_id")
            .cloned()
            .and_then(|id| serde_json::from_value::<SessionId>(id).ok())
        else {
            report.dropped_lines += 1;
            continue;
        };
        sessions
            .entry(session_id)
            .or_default()
            .extend_from_slice(line);
        report.lines += 1;
    }
    report.sessions = sessions.len();

    let staging = staging_dir(dir);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("remove an unfinished split {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    for (session_id, lines) in sessions {
        let path = super::session_path(&staging, session_id);
        let mut file =
            std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        file.write_all(&lines)
            .and_then(|()| file.sync_all())
            .with_context(|| format!("write {}", path.display()))?;
    }
    if dir.exists() {
        std::fs::remove_dir(dir).with_context(|| {
            format!(
                "replace {} (it must be empty to take the split)",
                dir.display()
            )
        })?;
    }
    std::fs::rename(&staging, dir)
        .with_context(|| format!("move {} to {}", staging.display(), dir.display()))?;
    Ok(report)
}

/// Where a split is written before it takes `dir`'s name.
fn staging_dir(dir: &Path) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".splitting");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::event_log::{read_session, session_ids};

    fn line(session: &str, sequence: u64, version: u32) -> String {
        serde_json::json!({
            "schema": AGENT_EVENT_LOG_SCHEMA,
            "version": version,
            "event_id": format!("{session}-{sequence}"),
            "parent_id": null,
            "sequence": sequence,
            "session_id": session,
            "turn_id": null,
            "provider_id": null,
            "role_id": null,
            "session_context": null,
            "event_kind": "state_changed",
            "event": {"StateChanged": "Running"},
            "provider_payload": null,
            "created_at_unix_ms": sequence,
        })
        .to_string()
    }

    #[test]
    fn each_session_gets_its_lines_in_their_order_and_the_rest_is_left_behind() {
        let home = tempfile::tempdir().unwrap();
        let (a, b) = (SessionId::new(), SessionId::new());
        let (a_text, b_text) = (a.as_uuid().to_string(), b.as_uuid().to_string());
        let legacy = home.path().join("agent-events.jsonl");
        std::fs::write(
            &legacy,
            format!(
                "{}\n{}\nnot json\n{}\n{{\"torn\":",
                line(&a_text, 3, AGENT_EVENT_LOG_VERSION),
                line(&b_text, 4, AGENT_EVENT_LOG_VERSION),
                line(&a_text, 9, AGENT_EVENT_LOG_VERSION),
            ),
        )
        .unwrap();
        let dir = home.path().join("sessions");

        let report = split_legacy_log(&legacy, &dir).unwrap();

        assert_eq!(
            report,
            SplitReport {
                sessions: 2,
                lines: 3,
                dropped_lines: 1,
                ignored_partial_line: true,
            }
        );
        assert_eq!(session_ids(&dir).unwrap().len(), 2);
        let sequences: Vec<u64> = read_session(&dir, a)
            .unwrap()
            .records
            .iter()
            .map(|record| record.sequence)
            .collect();
        assert_eq!(sequences, [3, 9]);
        assert!(legacy.exists(), "the shared log is left for the caller");
        assert!(!staging_dir(&dir).exists());
    }

    #[test]
    fn a_log_of_another_version_is_refused_and_nothing_is_written() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("agent-events.jsonl");
        let session = SessionId::new().as_uuid().to_string();
        std::fs::write(&legacy, format!("{}\n", line(&session, 0, 1))).unwrap();
        let dir = home.path().join("sessions");

        let error = split_legacy_log(&legacy, &dir).unwrap_err();

        assert!(error.is::<UnsupportedEventLogFormat>(), "{error:#}");
        assert!(!dir.exists());
    }
}
