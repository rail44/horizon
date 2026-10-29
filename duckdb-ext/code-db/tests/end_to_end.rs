//! `code-db` on a small fixture repository: `index` finds its two Cargo
//! workspaces and indexes each with rust-analyzer, and hands jscpd (a stand-in
//! here) the source files its `.code-db.toml` keeps in scope; the queries then
//! show the layer violation, the cycle's violating_edges and the settings'
//! test rules, and a query that reads no clone runs without the reports.
//!
//! Needs `duckdb-ext/build.sh` (the extensions) and rust-analyzer on PATH
//! (`RUST_ANALYZER` overrides it); without rust-analyzer the test is skipped,
//! as check.py's fixture is.

use std::path::Path;
use std::process::Command;

const SETTINGS: &str = r#"
[scope]
exclude = [ { path = "src/generated.rs", reason = "written by a tool" } ]

[tests]
paths = [ { path = "src/checks_*.rs", reason = "this fixture's check files" } ]
extra_attributes = [ { name = "inline", reason = "this fixture marks its checks with #[inline]" } ]

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

const LIB: &str =
    "pub mod checks_extra;\npub mod generated;\npub mod model;\npub mod util;\npub mod view;\n";
const MODEL: &str = "\
pub fn base() -> u32 {
    crate::view::width() + 1
}

#[inline]
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
const GENERATED: &str = "pub fn generated() -> u32 {\n    4\n}\n";
const CARGO_TOML: &str =
    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
const TOOL_TOML: &str =
    "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
const TOOL_MAIN: &str = "fn main() {\n    run();\n}\n\nfn run() {}\n";

/// A stand-in for jscpd: records its arguments and writes a report in jscpd
/// 5.3.1's JSON shape (--absolute) with one pair into its --output directory.
const FAKE_JSCPD: &str = r#"#!/bin/sh
printf '%s\n' "$@" >> "$FAKE_JSCPD_LOG"
while [ $# -gt 0 ]; do
    if [ "$1" = --output ]; then out=$2; fi
    shift
done
mkdir -p "$out"
cat > "$out/jscpd-report.json" <<EOF
{"duplicates": [{"format": "rust", "lines": 3, "tokens": 20, "fragment": "", "isNew": false, "kind": "exact",
 "firstFile": {"name": "$PWD/src/view.rs", "start": 1, "end": 3},
 "secondFile": {"name": "$PWD/src/util.rs", "start": 1, "end": 3}}], "statistics": {}}
EOF
"#;

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

fn run(repo: &Path, args: &[&str], jscpd: &Path, log: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_code-db"))
        .args(args.iter().take(1))
        .arg(repo)
        .args(args.iter().skip(1))
        .env("JSCPD", jscpd)
        .env("FAKE_JSCPD_LOG", log)
        .output()
        .unwrap()
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
fn index_settings_and_queries() {
    let ra = std::env::var("RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_owned());
    if Command::new(&ra).arg("--version").output().is_err() {
        eprintln!("SKIPPED: {ra} not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let repo = repo.as_path();
    git(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
    write(repo, "Cargo.toml", CARGO_TOML);
    write(repo, ".gitignore", "target/\n");
    write(repo, "src/lib.rs", LIB);
    write(repo, "src/model.rs", MODEL);
    write(repo, "src/view.rs", VIEW);
    write(repo, "src/util.rs", UTIL);
    write(repo, "src/checks_extra.rs", CHECKS);
    write(repo, "src/generated.rs", GENERATED);
    write(repo, "tool/Cargo.toml", TOOL_TOML);
    write(repo, "tool/src/main.rs", TOOL_MAIN);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add the fixture"]);
    // Not committed: the settings file is read from the worktree.
    write(repo, ".code-db.toml", SETTINGS);
    let jscpd = tmp.path().join("jscpd");
    write(tmp.path(), "jscpd", FAKE_JSCPD);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&jscpd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = tmp.path().join("jscpd.log");
    let code_db = |args: &[&str]| {
        let output = run(repo, args, &jscpd, &log);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "code-db {args:?}: {stderr}");
        String::from_utf8(output.stdout).unwrap()
    };

    code_db(&["index"]);
    let scip = repo.join("target/code-db/scip");
    let mut indexes: Vec<String> = std::fs::read_dir(&scip)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    indexes.sort();
    assert_eq!(indexes, ["root.scip", "ws-tool.scip"]);
    // jscpd ran twice (exact, normalized) over the files in scope.
    let args = std::fs::read_to_string(&log).unwrap();
    let scanned: Vec<&str> = args.lines().filter(|a| a.ends_with(".rs")).collect();
    let in_scope = [
        "src/checks_extra.rs",
        "src/lib.rs",
        "src/model.rs",
        "src/util.rs",
        "src/view.rs",
        "tool/src/main.rs",
    ];
    assert_eq!(scanned, [in_scope, in_scope].concat());

    let violations = rows(&code_db(&["query", "layer_violations"]));
    assert_eq!(
        violations
            .iter()
            .map(|r| (
                &r[0][..],
                &r[1][..],
                &r[2][..],
                &r[3][..],
                &r[4][..],
                &r[9][..]
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

    let cycles = rows(&code_db(&["query", "module_cycles"]));
    let summary: Vec<(&str, &str, &str, &str)> = cycles
        .iter()
        .map(|r| (&r[0][..], &r[3][..], &r[4][..], &r[11][..]))
        .collect();
    assert_eq!(
        summary,
        [
            ("src/model.rs", "src/model.rs", "src/view.rs", "1"),
            ("src/model.rs", "src/view.rs", "src/model.rs", "1"),
        ]
    );

    // The settings' test rules: the function with #[inline] and the file
    // src/checks_extra.rs, besides Rust's own rules. The excluded file has no
    // units; the second workspace's have.
    let probe = repo.join("target/probe.sql");
    std::fs::write(
        &probe,
        "SELECT path, kind, qualified_name, is_test FROM units \
         WHERE is_test OR path IN ('src/generated.rs', 'tool/src/main.rs') ORDER BY ALL;\n",
    )
    .unwrap();
    let tests = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    let cells: Vec<Vec<&str>> = tests
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        cells,
        [
            vec!["src/checks_extra.rs", "function", "probe", "true"],
            vec!["src/checks_extra.rs", "module", "checks_extra", "true"],
            vec!["src/model.rs", "function", "checked", "true"],
            vec!["tool/src/main.rs", "function", "main", "false"],
            vec!["tool/src/main.rs", "function", "run", "false"],
            vec!["tool/src/main.rs", "module", "tool", "false"],
        ]
    );

    // The stand-in writes its pair into both reports.
    let pairs = rows(&code_db(&["query", "duplication"]));
    assert_eq!(
        pairs
            .iter()
            .map(|r| (&r[8][..], &r[14][..]))
            .collect::<Vec<_>>(),
        [("width", "helper"), ("width", "helper")]
    );

    // Without the reports, a query that reads no clone still runs; one that does
    // says to run index.
    std::fs::remove_dir_all(repo.join("target/code-db/jscpd")).unwrap();
    let sizes = rows(&code_db(&["query", "size", "level=crate"]));
    assert_eq!(sizes.len(), 2);
    let output = run(repo, &["query", "duplication"], &jscpd, &log);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("duplication needs the jscpd report") && stderr.contains("code-db index"),
        "{stderr}"
    );
}
