//! `code-db` on a small fixture repository: its `.code-db.toml` declares two
//! layers and the settings' test rules, rust-analyzer indexes it, and the
//! queries show the layer violation, the cycle's violating_edges and the
//! extra test tags.
//!
//! Needs `duckdb-ext/build.sh` (the extensions) and rust-analyzer on PATH
//! (`RUST_ANALYZER` overrides it); without rust-analyzer the test is skipped,
//! as check.py's SCIP fixtures are.

use std::path::Path;
use std::process::Command;

const SETTINGS: &str = r#"
[tests]
paths = [ { path = "src/checks_*.rs", reason = "this fixture's check files" } ]
extra_attributes = [ { name = "my_check", reason = "this fixture's test attribute" } ]

[[layers]]
name = "model"
modules = ["src/model.rs*"]
reason = "the data, below the view"

[[layers]]
name = "view"
modules = ["src/view.rs*"]
may_depend_on = ["model"]
reason = "the view reads the model"
"#;

const LIB: &str = "pub mod checks_extra;\npub mod model;\npub mod util;\npub mod view;\n";
const MODEL: &str = "\
pub fn base() -> u32 {
    crate::view::width() + 1
}

#[my_check]
fn checked() {}
";
const VIEW: &str = "\
pub fn width() -> u32 {
    2
}

pub fn draw() -> u32 {
    crate::model::base()
}
";
const UTIL: &str = "\
pub fn helper() -> u32 {
    crate::view::width()
}
";
const CHECKS: &str = "pub fn probe() -> u32 {\n    3\n}\n";
const CARGO_TOML: &str =
    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";

fn git(dir: &Path, args: &[&str]) {
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
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A report in jscpd 5.3.1's JSON shape (--absolute) with one pair.
fn jscpd_report(repo: &Path) -> String {
    let side = |path: &str| {
        format!(
            r#"{{"name": "{}", "start": 1, "end": 3, "startLoc": {{"line": 1, "column": 1, "position": 0}}, "endLoc": {{"line": 3, "column": 1, "position": 0}}}}"#,
            repo.join(path).display()
        )
    };
    format!(
        r#"{{"duplicates": [{{"format": "rust", "lines": 3, "tokens": 20, "fragment": "", "isNew": false, "kind": "exact", "firstFile": {}, "secondFile": {}}}], "statistics": {{}}}}"#,
        side("src/view.rs"),
        side("src/util.rs")
    )
}

fn code_db(repo: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_code-db"))
        .args(args.iter().take(1))
        .arg(repo)
        .args(args.iter().skip(1))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "code-db {args:?}: {stderr}");
    assert!(stderr.starts_with("limits: memory "), "{stderr}");
    String::from_utf8(output.stdout).unwrap()
}

/// The data rows of a markdown table, cells trimmed.
fn rows(markdown: &str) -> Vec<Vec<String>> {
    markdown
        .lines()
        .skip(2)
        .map(|l| {
            l.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn layers_and_test_settings_reach_the_queries() {
    let ra = std::env::var("RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_owned());
    if Command::new(&ra).arg("--version").output().is_err() {
        eprintln!("SKIPPED: {ra} not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "-b", "main"]);
    write(repo, "Cargo.toml", CARGO_TOML);
    write(repo, ".gitignore", "target/\n");
    write(repo, "src/lib.rs", LIB);
    write(repo, "src/model.rs", MODEL);
    write(repo, "src/view.rs", VIEW);
    write(repo, "src/util.rs", UTIL);
    write(repo, "src/checks_extra.rs", CHECKS);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add the fixture"]);
    // Not committed: the settings file is read from the worktree.
    write(repo, ".code-db.toml", SETTINGS);
    let out = repo.join("target/code-db");
    for mode in ["exact", "normalized"] {
        write(
            &out,
            &format!("jscpd/{mode}/jscpd-report.json"),
            &jscpd_report(repo),
        );
    }
    let status = Command::new(&ra)
        .args(["scip", ".", "--output"])
        .arg(out.join("index.scip"))
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");

    let violations = rows(&code_db(repo, &["query", "layer_violations"]));
    assert_eq!(
        violations
            .iter()
            .map(|r| (
                &r[0][..],
                &r[1][..],
                &r[2][..],
                &r[3][..],
                &r[4][..],
                &r[10][..]
            ))
            .collect::<Vec<_>>(),
        [(
            "model",
            "view",
            "src/model.rs",
            "src/view.rs",
            "1",
            "src/model.rs"
        )]
    );

    let cycles = rows(&code_db(repo, &["query", "module_cycles"]));
    let summary: Vec<(&str, &str, &str, &str)> = cycles
        .iter()
        .map(|r| (&r[0][..], &r[3][..], &r[4][..], &r[12][..]))
        .collect();
    assert_eq!(
        summary,
        [
            ("src/model.rs", "src/model.rs", "src/view.rs", "1"),
            ("src/model.rs", "src/view.rs", "src/model.rs", "1"),
        ]
    );

    // The settings' test rules tag the function with #[my_check] and the file
    // src/checks_extra.rs, besides lang/rust.sql's own rules.
    let probe = repo.join("target/probe.sql");
    std::fs::write(
        &probe,
        "SELECT f.path, t.start_line FROM tags t JOIN files f USING (file_id) \
         WHERE t.tag = 'test' ORDER BY ALL;\n",
    )
    .unwrap();
    let tests = rows(&code_db(repo, &["sql", probe.to_str().unwrap()]));
    assert_eq!(
        tests,
        [
            vec!["src/checks_extra.rs".to_owned(), "1".to_owned()],
            vec!["src/model.rs".to_owned(), "6".to_owned()],
        ]
    );
}
