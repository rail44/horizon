//! Offline conversion writes a separate bundle and never activates it.
use super::{upgrade_conversation_records, Record};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};

/// The source must belong to a stopped writer. The destination must not exist.
/// Validate everything before creating the bundle; keep the source byte-for-byte.
pub fn convert_conversation_file(source: &Path, destination: &Path) -> Result<usize> {
    anyhow::ensure!(
        !destination.exists(),
        "destination already exists: {}",
        destination.display()
    );
    let original = fs::read(source).with_context(|| format!("read {}", source.display()))?;
    anyhow::ensure!(
        original.is_empty() || original.ends_with(b"\n"),
        "source has an incomplete final line"
    );
    let rows: Vec<Value> = std::str::from_utf8(&original)?
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| format!("decode source line {}", index + 1))
        })
        .collect::<Result<_>>()?;
    let converted = upgrade_conversation_records(&rows).map_err(anyhow::Error::msg)?;
    let records: Vec<Record> = converted
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
    let store = crate::persistence::projection::duckdb::Store::open_in_memory()?;
    let imported = store.replace_from_event_log_records(records)?;
    anyhow::ensure!(
        imported.applied == converted.len() && imported.skipped == 0,
        "projection rejected converted records: {:?}",
        imported.first_skip_error
    );
    let mut output = Vec::new();
    for record in &converted {
        serde_json::to_writer(&mut output, record)?;
        output.push(b'\n');
    }
    anyhow::ensure!(
        fs::read(source)? == original,
        "source changed during conversion; stop its writer first"
    );
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(destination)?;
    let write = |name: &str, bytes: &[u8]| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    };
    write("original.jsonl", &original)?;
    write("events.jsonl", &output)?;
    write(
        "manifest.json",
        &serde_json::to_vec_pretty(&json!({
            "source": source, "source_records": rows.len(), "converted_records": converted.len(),
            "target_version": 4, "activated": false,
            "limitations": "Only metadata present in the source can be recovered. Legacy reasoning signatures absent from v3 cannot be recreated."
        }))?,
    )?;
    #[cfg(unix)]
    fs::File::open(destination)?.sync_all()?;
    Ok(converted.len())
}
