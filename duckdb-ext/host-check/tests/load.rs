//! LOAD the built `git_history.duckdb_extension` through the `duckdb` crate
//! and query `git_hunks` on a fixture repository.
//!
//! Run `duckdb-ext/build.sh` first; `GIT_HISTORY_EXTENSION` overrides the
//! extension path.

use std::path::{Path, PathBuf};
use std::process::Command;

use duckdb::{Config, Connection};

fn extension() -> PathBuf {
    let path = std::env::var_os("GIT_HISTORY_EXTENSION")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/extension/v2.0.0/linux_amd64/git_history.duckdb_extension")
        });
    assert!(
        path.exists(),
        "{} is missing; run duckdb-ext/build.sh first",
        path.display()
    );
    path.canonicalize().unwrap()
}

fn git(dir: &Path, args: &[&str], date: &str) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn horizon_linked_duckdb_loads_the_extension() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"], "@1700000000 +0000");
    std::fs::write(dir.join("a.rs"), "one\ntwo\nthree\n").unwrap();
    git(dir, &["add", "-A"], "@1700000000 +0000");
    git(
        dir,
        &["commit", "-q", "-m", "Add a.rs"],
        "@1700000000 +0000",
    );
    std::fs::write(dir.join("a.rs"), "one\n2\nthree\nfour\n").unwrap();
    git(
        dir,
        &["commit", "-q", "-am", "Edit a.rs"],
        "@1700003600 +0000",
    );

    let config = Config::default().allow_unsigned_extensions().unwrap();
    let conn = Connection::open_in_memory_with_flags(config).unwrap();
    let version: String = conn
        .query_row("SELECT library_version FROM pragma_version()", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, "v2.0.0");
    conn.execute_batch(&format!("LOAD '{}'", extension().display()))
        .unwrap();

    let mut statement = conn
        .prepare(
            "SELECT c.subject, h.path, h.old_start, h.old_lines, h.new_start, h.new_lines
             FROM git_hunks(?) h JOIN git_commits(?) c USING (sha)
             ORDER BY c.author_time, h.old_start",
        )
        .unwrap();
    let path = dir.to_str().unwrap();
    let rows: Vec<(String, String, i32, i32, i32, i32)> = statement
        .query_map([path, path], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let rows: Vec<(&str, &str, i32, i32, i32, i32)> = rows
        .iter()
        .map(|(s, p, a, b, c, d)| (s.as_str(), p.as_str(), *a, *b, *c, *d))
        .collect();
    assert_eq!(
        rows,
        [
            ("Add a.rs", "a.rs", 0, 0, 1, 3),
            ("Edit a.rs", "a.rs", 2, 1, 2, 1),
            ("Edit a.rs", "a.rs", 3, 0, 4, 1),
        ]
    );
}
