pub mod event_log;
pub mod projection;

/// Read and rebuild the session logs in `dir` without opening any live
/// projection or writer. Unlike runtime recovery, preflight accepts no
/// skipped records. Sessions are checked in the order they began, so a
/// fork's source comes before it.
pub fn validate_history(dir: impl AsRef<std::path::Path>) -> anyhow::Result<usize> {
    let dir = dir.as_ref();
    anyhow::ensure!(
        dir.is_dir(),
        "session log directory does not exist: {}",
        dir.display()
    );
    let mut report = event_log::ReadReport::default();
    let mut sessions = Vec::new();
    for session_id in event_log::session_ids(dir)? {
        let session = event_log::read_session(dir, session_id)?;
        if let Some(summary) = session.skipped_summary() {
            anyhow::bail!("history is not ready for activation: {summary}");
        }
        sessions.push(session.records);
    }
    sessions.sort_by_key(|records| {
        records
            .first()
            .map(|first| (first.created_at_unix_ms, first.session_id.as_uuid()))
    });
    report.records = sessions.into_iter().flatten().collect();
    crate::providers::rig::conversation::validate::validate_conversation_records(
        &report
            .records
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?,
    )
    .map_err(anyhow::Error::msg)?;
    let expected = report.records.len();
    let store = projection::duckdb::Store::open_in_memory()?;
    let imported = store.replace_from_event_log_records(report.records)?;
    anyhow::ensure!(
        imported.skipped == 0 && imported.applied == expected,
        "projection imported {} of {} records; first error: {:?}",
        imported.applied,
        expected,
        imported.first_skip_error,
    );
    Ok(expected)
}
