//! The probe binary; usage is in the crate docs (`src/lib.rs`).

use std::time::Instant;

use gix_probe::*;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut options = Options::default();
    let mut stats = false;
    let mut rest = args[2..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--since" => options.since = rest.next().cloned(),
            "--include-merges" => options.include_merges = true,
            "--sorting" => {
                options.sorting = Sorting::parse(rest.next().ok_or("--sorting needs a value")?)?
            }
            "--slider" => {
                options.slider = match rest.next().map(String::as_str) {
                    Some("indent") => Slider::Indent,
                    Some("none") => Slider::None,
                    Some("raw") => Slider::Raw,
                    other => return Err(format!("unknown slider {other:?}").into()),
                }
            }
            "--unified-header" => options.unified_header = true,
            "--context" => {
                options.context = Some(rest.next().ok_or("--context needs a value")?.parse()?)
            }
            "--no-commit-graph" => options.no_commit_graph = true,
            "--track-empty" => options.track_empty = true,
            "--stats" => stats = true,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let start = Instant::now();
    let repo = open(&args[0])?;
    let null = |v: &Option<String>| v.clone().unwrap_or_else(|| "\\N".into());
    let num = |v: Option<i32>| v.map_or_else(|| "\\N".into(), |v| v.to_string());
    let mut out = String::new();
    let count;
    match args[1].as_str() {
        "hunks" => {
            let rows = hunks(&repo, &options)?;
            count = rows.len();
            for h in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    h.sha, h.path, h.old_start, h.old_lines, h.new_start, h.new_lines
                );
            }
        }
        "commits" => {
            let rows = commits(&repo, &options)?;
            count = rows.len();
            for c in rows {
                out += &format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    c.sha, c.author_time, c.author_name, c.subject, c.parent_count, c.is_merge
                );
            }
        }
        "file_changes" => {
            let rows = file_changes(&repo, &options)?;
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
        other => return Err(format!("unknown table {other}").into()),
    }
    let elapsed = start.elapsed();
    use std::io::Write;
    std::io::stdout().write_all(out.as_bytes())?;
    if stats {
        eprintln!("{} rows in {:.3} s", count, elapsed.as_secs_f64());
    }
    Ok(())
}
