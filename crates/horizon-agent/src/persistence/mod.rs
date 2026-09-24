pub mod event_log;
pub mod projection;

/// Read and rebuild a converted log without opening any live projection or
/// writer. Unlike runtime recovery, preflight accepts no skipped records.
pub fn validate_history(path: impl AsRef<std::path::Path>) -> anyhow::Result<usize> {
    let path = path.as_ref();
    anyhow::ensure!(
        path.is_file(),
        "history file does not exist: {}",
        path.display()
    );
    let report = event_log::read(path)?;
    if let Some(summary) = report.skipped_summary() {
        anyhow::bail!("history is not ready for activation: {summary}");
    }
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
