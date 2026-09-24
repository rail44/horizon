//! Offline migration preflight using the actual reader and DuckDB projection.

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: validate-history EVENTS_JSONL"))?;
    anyhow::ensure!(
        args.next().is_none(),
        "usage: validate-history EVENTS_JSONL"
    );
    let count = horizon_agent::persistence::validate_history(std::path::PathBuf::from(path))?;
    println!("Validated {count} records; no skipped records or projection errors.");
    Ok(())
}
