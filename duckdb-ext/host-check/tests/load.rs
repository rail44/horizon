//! LOAD the built `git_history.duckdb_extension` and
//! `scip_index.duckdb_extension` through the `duckdb` crate: query
//! `git_tracked_files` on a fixture repository, and the three SCIP functions
//! on a hand-encoded index (one path and a list of paths), with both
//! extensions loaded.
//!
//! Run `duckdb-ext/build.sh` first; `GIT_HISTORY_EXTENSION` and
//! `SCIP_INDEX_EXTENSION` override the extension paths.

use std::path::{Path, PathBuf};
use std::process::Command;

use duckdb::{Config, Connection};

fn extension(name: &str, variable: &str) -> PathBuf {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../target/extension/v2.0.0/linux_amd64/{name}.duckdb_extension"
            ))
        });
    assert!(
        path.exists(),
        "{} is missing; run duckdb-ext/build.sh first",
        path.display()
    );
    path.canonicalize().unwrap()
}

fn git_history() -> PathBuf {
    extension("git_history", "GIT_HISTORY_EXTENSION")
}

fn scip_index() -> PathBuf {
    extension("scip_index", "SCIP_INDEX_EXTENSION")
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
    conn.execute_batch(&format!("LOAD '{}'", git_history().display()))
        .unwrap();
    let path = dir.to_str().unwrap();

    std::fs::write(dir.join("a.rs"), "changed\n").unwrap();
    std::fs::write(dir.join("b.rs"), "new\n").unwrap();
    let mut statement = conn
        .prepare(
            "SELECT path, mode, length(blob_id), is_tracked, is_modified
             FROM git_tracked_files(?, include_untracked := true)",
        )
        .unwrap();
    let rows: Vec<(String, String, i64, bool, bool)> = statement
        .query_map([path], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let rows: Vec<(&str, &str, i64, bool, bool)> = rows
        .iter()
        .map(|(p, m, l, t, d)| (p.as_str(), m.as_str(), *l, *t, *d))
        .collect();
    assert_eq!(
        rows,
        [
            ("a.rs", "100644", 40, true, true),
            ("b.rs", "100644", 40, false, false),
        ]
    );
}

/// Protobuf field encoders for the hand-encoded index.
fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn int_field(number: u64, value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, number << 3);
    varint(&mut out, value);
    out
}

fn len_field(number: u64, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, (number << 3) | 2);
    varint(&mut out, value.len() as u64);
    out.extend_from_slice(value);
    out
}

#[test]
fn horizon_linked_duckdb_loads_both_extensions() {
    // A definition with a packed range and an enclosing range, and a local
    // reference with a typed multi-line range and no enclosing range.
    let definition = [
        len_field(1, &[2, 4, 9]),
        len_field(2, b"cargo p 0.1.0 f()."),
        int_field(3, 0x1),
        len_field(7, &[1, 0, 3, 1]),
    ]
    .concat();
    let multi = [
        int_field(1, 3),
        int_field(2, 1),
        int_field(3, 4),
        int_field(4, 2),
    ]
    .concat();
    let reference = [
        len_field(9, &multi),
        len_field(2, b"local 0"),
        int_field(3, 0x8),
    ]
    .concat();
    let symbol = [
        len_field(1, b"cargo p 0.1.0 f()."),
        len_field(3, b"Docs."),
        int_field(5, 17),
        len_field(6, b"f"),
        len_field(7, &len_field(5, b"pub fn f()")),
    ]
    .concat();
    let document = [
        len_field(1, b"src/lib.rs"),
        len_field(2, &definition),
        len_field(2, &reference),
        len_field(3, &symbol),
    ]
    .concat();
    let metadata = [
        len_field(2, &[len_field(1, b"hand"), len_field(3, b"scip")].concat()),
        int_field(4, 1),
    ]
    .concat();
    let index = [
        len_field(1, &metadata),
        len_field(2, &document),
        len_field(3, &len_field(1, b"std io/Read#")),
    ]
    .concat();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.scip");
    std::fs::write(&path, index).unwrap();
    let path = path.to_str().unwrap();

    let config = Config::default().allow_unsigned_extensions().unwrap();
    let conn = Connection::open_in_memory_with_flags(config).unwrap();
    conn.execute_batch(&format!(
        "LOAD '{}'; LOAD '{}'",
        git_history().display(),
        scip_index().display()
    ))
    .unwrap();

    let mut statement = conn
        .prepare(
            "SELECT document, symbol, start_line, start_char, end_line, end_char, roles,
                    is_definition, is_read, enclosing_end_line
             FROM read_scip(?)",
        )
        .unwrap();
    type Occurrence = (
        String,
        String,
        i32,
        i32,
        i32,
        i32,
        i32,
        bool,
        bool,
        Option<i32>,
    );
    let rows: Vec<Occurrence> = statement
        .query_map([path], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let file = "src/lib.rs".to_string();
    assert_eq!(
        rows,
        [
            (
                file.clone(),
                "cargo p 0.1.0 f().".into(),
                2,
                4,
                2,
                9,
                1,
                true,
                false,
                Some(3)
            ),
            (file, "local 0".into(), 3, 1, 4, 2, 8, false, true, None),
        ]
    );

    let mut statement = conn
        .prepare(
            "SELECT coalesce(document, '<external>'), symbol, kind, documentation::VARCHAR,
                    coalesce(signature, '<none>')
             FROM read_scip_symbols(?)",
        )
        .unwrap();
    let rows: Vec<(String, String, String, String, String)> = statement
        .query_map([path], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let rows: Vec<(&str, &str, &str, &str, &str)> = rows
        .iter()
        .map(|(d, s, k, doc, sig)| {
            (
                d.as_str(),
                s.as_str(),
                k.as_str(),
                doc.as_str(),
                sig.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "src/lib.rs",
                "cargo p 0.1.0 f().",
                "Function",
                "[Docs.]",
                "pub fn f()"
            ),
            (
                "<external>",
                "std io/Read#",
                "UnspecifiedKind",
                "[]",
                "<none>"
            ),
        ]
    );

    // A list of paths reads each index in order; index_path names the file.
    let list = format!("['{path}', '{path}']");
    let (occurrences, symbols, metadata, paths): (i64, i64, i64, i64) = conn
        .query_row(
            &format!(
                "SELECT (SELECT count(*) FROM read_scip({list})),
                        (SELECT count(*) FROM read_scip_symbols({list})),
                        (SELECT count(*) FROM read_scip_metadata({list})),
                        (SELECT count(*) FROM read_scip({list}) WHERE index_path = '{path}')"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((occurrences, symbols, metadata, paths), (4, 4, 2, 4));
    let empty: i64 = conn
        .query_row("SELECT count(*) FROM read_scip([]::VARCHAR[])", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(empty, 0);

    let row: (String, String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT tool_name, tool_arguments::VARCHAR, text_document_encoding,
                    document_count, occurrence_count, external_symbol_count
             FROM read_scip_metadata(?)",
            [path],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        ("hand".into(), "[scip]".into(), "UTF8".into(), 1, 2, 1)
    );

    // A missing index fails at bind time, naming the path.
    let error = conn
        .execute_batch("SELECT * FROM read_scip('/nonexistent/index.scip')")
        .unwrap_err();
    assert!(
        error.to_string().contains("/nonexistent/index.scip"),
        "{error}"
    );
}
