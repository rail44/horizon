//! Print one function's rows as TSV, for comparing with `git` CLI output.
//!
//! cargo run --release --example dump -- REPO hunks|commits|file_changes [SINCE] [--include-merges]

use git_history::history::{self, Range};

fn main() -> Result<(), git2::Error> {
    let args: Vec<String> = std::env::args().collect();
    let repo = history::open(&args[1])?;
    let range = Range {
        since: args.get(3).filter(|a| !a.starts_with("--")).cloned(),
        include_merges: args.iter().any(|a| a == "--include-merges"),
    };
    let null = |v: &Option<String>| v.clone().unwrap_or_else(|| "\\N".into());
    let num = |v: Option<i32>| v.map_or_else(|| "\\N".into(), |v| v.to_string());
    match args[2].as_str() {
        "hunks" => {
            for h in history::hunks(&repo, &range)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    h.sha, h.path, h.old_start, h.old_lines, h.new_start, h.new_lines
                );
            }
        }
        "commits" => {
            for c in history::commits(&repo, &range)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    c.sha, c.author_time, c.author_name, c.subject, c.parent_count, c.is_merge
                );
            }
        }
        "file_changes" => {
            for f in history::file_changes(&repo, &range)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    f.sha,
                    f.path,
                    null(&f.old_path),
                    f.status,
                    num(f.added),
                    num(f.deleted)
                );
            }
        }
        other => panic!("unknown table {other}"),
    }
    Ok(())
}
