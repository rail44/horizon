//! Print the rows of `git_hunks`, `git_file_changes`, `git_commits`,
//! `git_tracked_files` or `git_changed_blobs` as TSV,
//! computed by `git_history_core` without DuckDB, for comparing with the git
//! CLI (`scripts/gix_parity.py`).
//!
//! Usage: gix-probe REPO hunks|file_changes|commits|tracked_files|changed_blobs [options]
//!
//! Options:
//!   --since REV          history is REV..HEAD
//!   --include-merges     diff merges against their first parent
//!   --context N          hunks with N context lines (`-U<N>`) instead of 0
//!   --include-untracked  tracked_files also lists untracked, not ignored files
//!   --dir DIR            changed_blobs writes the blobs to DIR (absolute, required)
//!   --extensions A,B     changed_blobs considers only paths with these extensions
//!   --stats              print the row count and total time to stderr, and for
//!                        changed_blobs the files written and skipped

use std::io::Write;
use std::time::Instant;

use git_history_core::{self as history, Range, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "usage: gix-probe REPO hunks|file_changes|commits|tracked_files|changed_blobs [options]"
                .into(),
        );
    }
    let mut range = Range::default();
    let mut context = 0;
    let mut stats = false;
    let mut include_untracked = false;
    let mut dir: Option<String> = None;
    let mut extensions: Option<Vec<String>> = None;
    let mut rest = args[2..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--since" => range.since = Some(rest.next().ok_or("--since needs a value")?.clone()),
            "--include-merges" => range.include_merges = true,
            "--context" => context = rest.next().ok_or("--context needs a value")?.parse()?,
            "--stats" => stats = true,
            "--include-untracked" => include_untracked = true,
            "--dir" => dir = Some(rest.next().ok_or("--dir needs a value")?.clone()),
            "--extensions" => {
                let list = rest.next().ok_or("--extensions needs a value")?;
                extensions = Some(list.split(',').map(str::to_string).collect());
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let start = Instant::now();
    let repo = history::open(&args[0])?;
    let null = |v: &Option<String>| v.clone().unwrap_or_else(|| "\\N".into());
    let num = |v: Option<i32>| v.map_or_else(|| "\\N".into(), |v| v.to_string());
    let mut out = String::new();
    let count;
    let mut blob_stats = None;
    match args[1].as_str() {
        "hunks" => {
            let rows = history::hunks_with_context(&repo, &range, context)?;
            count = rows.len();
            for h in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    h.sha, h.path, h.old_start, h.old_lines, h.new_start, h.new_lines
                );
            }
        }
        "commits" => {
            let rows = history::commits(&repo, &range)?;
            count = rows.len();
            for c in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    c.sha, c.author_time, c.author_name, c.subject, c.parent_count, c.is_merge
                );
            }
        }
        "file_changes" => {
            let rows = history::file_changes(&repo, &range)?;
            count = rows.len();
            for f in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    f.sha,
                    f.path,
                    null(&f.old_path),
                    f.status,
                    num(f.added),
                    num(f.deleted)
                );
            }
        }
        "tracked_files" => {
            let rows = history::tracked_files(&repo, include_untracked)?;
            count = rows.len();
            for t in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    t.path, t.mode, t.blob_id, t.is_tracked, t.is_modified
                );
            }
        }
        "changed_blobs" => {
            let dir = dir.ok_or("changed_blobs needs --dir")?;
            let (rows, written) = history::changed_blobs(
                &repo,
                &range,
                std::path::Path::new(&dir),
                extensions.as_deref(),
            )?;
            count = rows.len();
            blob_stats = Some(written);
            for b in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    b.sha,
                    b.path,
                    null(&b.old_path),
                    b.side,
                    b.blob_id,
                    b.mode,
                    b.file
                );
            }
        }
        other => return Err(format!("unknown table {other}").into()),
    }
    let elapsed = start.elapsed();
    std::io::stdout().write_all(out.as_bytes())?;
    if stats {
        eprintln!("{} rows in {:.3} s", count, elapsed.as_secs_f64());
        if let Some(s) = blob_stats {
            eprintln!(
                "{} files written ({} bytes), {} skipped",
                s.written, s.bytes_written, s.skipped
            );
        }
    }
    Ok(())
}
