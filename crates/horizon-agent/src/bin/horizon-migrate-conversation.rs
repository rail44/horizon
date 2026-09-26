//! Explicit offline format conversion; no runtime log is opened for append.
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "usage: horizon-migrate-conversation <stopped-v3-or-v4.jsonl> <new-output-directory>"
    );
    let count = horizon_agent::persistence::event_log::convert_conversation_file(
        std::path::Path::new(&args[0]),
        std::path::Path::new(&args[1]),
    )?;
    println!("Validated {count} records. Source preserved; converted bundle is not activated.");
    Ok(())
}
