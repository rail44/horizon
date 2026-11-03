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

const LIB: &str = "pub mod checks_extra;\npub mod generated;\npub mod model;\npub mod shape;\npub mod util;\npub mod view;\n";
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
const SHAPE: &str = "\
pub trait Area {
    fn area(&self) -> u32;
}

pub struct Square;

impl Square {
    pub fn new() -> Square {
        Square
    }
}

impl Area for Square {
    fn area(&self) -> u32 {
        1
    }
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
    write(repo, "src/shape.rs", SHAPE);
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
        "src/shape.rs",
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
        "SELECT path, kind, qualified_name, is_test FROM rust.units \
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

    // Rust's names for the vocabulary values, in their columns only: the query
    // `interface` keeps its name.
    std::fs::write(
        &probe,
        "SELECT u.qualified_name, coalesce(u.member_of, '-'), r.status \
         FROM rust.units u JOIN rust.unit_reference_status r USING (unit_id) \
         WHERE u.path = 'src/shape.rs' AND u.kind = 'method' ORDER BY ALL;\n",
    )
    .unwrap();
    let members = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    assert_eq!(
        members,
        [
            ["Area for Square.area", "trait_impl", "trait_impl"],
            ["Area.area", "trait", "trait_item"],
            ["Square.new", "impl", "unreferenced"],
        ]
    );
    assert!(!rows(&code_db(&["query", "interface"])).is_empty());

    // The stand-in writes its pair into both reports, which makes one pair, side a
    // the smaller path.
    let pairs = rows(&code_db(&["query", "duplication"]));
    assert_eq!(
        pairs
            .iter()
            .map(|r| (&r[1][..], &r[8][..], &r[14][..]))
            .collect::<Vec<_>>(),
        [("[exact, normalized]", "helper", "width")]
    );

    // Without the reports, a query that reads no clone still runs; one that does
    // says to run index.
    std::fs::remove_dir_all(repo.join("target/code-db/jscpd")).unwrap();
    let sizes = rows(&code_db(&["query", "size", "level=package"]));
    assert_eq!(sizes.len(), 2);
    let output = run(repo, &["query", "duplication"], &jscpd, &log);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("duplication needs the jscpd report") && stderr.contains("code-db index"),
        "{stderr}"
    );
}

const GO_MOD: &str = "module example.com/shapes\n\ngo 1.22\n";
const GO_SHAPES: &str = "\
// Package shapes computes areas.
package shapes

const (
	Small = 1
	Large = 2
)

// Shape has an area.
type Shape interface {
	Area() float64
}

type Circle struct {
	R float64
}

func (c Circle) Area() float64 {
	return 3 * c.R * c.R
}

func (c *Circle) scale(by, min float64) {
	c.R *= by
}

func Describe(s Shape) float64 {
	return s.Area()
}
";
const GO_SHAPES_TEST: &str = "\
package shapes

import \"testing\"

func TestArea(t *testing.T) {
	if (Circle{R: 1}).Area() <= 0 {
		t.Fatal(\"area\")
	}
}
";
const GO_MAIN: &str = "\
package main

import \"example.com/shapes/shapes\"

func main() {
	println(shapes.Describe(shapes.Circle{R: 2}))
}
";
const GO_TOOL_MOD: &str = "module example.com/gen\n\ngo 1.22\n";
const GO_TOOL_MAIN: &str = "\
package main

func main() {}

func grade(n int) string {
	switch {
	case n > 90:
		return \"a\"
	case n > 50 && n < 60:
		return \"b\"
	default:
	}
	for i := 0; i < n; i++ {
		if i == 3 {
			break
		}
	}
	return \"c\"
}
";

#[test]
fn go_modules_index_and_units() {
    let scip_go = std::env::var("SCIP_GO").unwrap_or_else(|_| "scip-go".to_owned());
    if Command::new(&scip_go).arg("--version").output().is_err() {
        eprintln!("SKIPPED: {scip_go} not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let repo = repo.as_path();
    git(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
    write(repo, ".gitignore", "target/\n");
    write(repo, "go.mod", GO_MOD);
    write(repo, "shapes/shapes.go", GO_SHAPES);
    write(repo, "shapes/shapes_test.go", GO_SHAPES_TEST);
    write(repo, "cmd/app/main.go", GO_MAIN);
    write(repo, "tools/gen/go.mod", GO_TOOL_MOD);
    write(repo, "tools/gen/main.go", GO_TOOL_MAIN);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add the fixture"]);
    let jscpd = tmp.path().join("jscpd");
    write(tmp.path(), "jscpd", FAKE_JSCPD);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&jscpd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = tmp.path().join("jscpd.log");
    let code_db = |args: &[&str]| {
        // No Cargo workspace: rust-analyzer is not needed.
        let output = Command::new(env!("CARGO_BIN_EXE_code-db"))
            .args(args.iter().take(1))
            .arg(repo)
            .args(args.iter().skip(1))
            .env("JSCPD", &jscpd)
            .env("FAKE_JSCPD_LOG", &log)
            .env("RUST_ANALYZER", "/nonexistent/rust-analyzer")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "code-db {args:?}: {stderr}");
        String::from_utf8(output.stdout).unwrap()
    };

    code_db(&["index"]);
    let mut indexes: Vec<String> = std::fs::read_dir(repo.join("target/code-db/scip"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    indexes.sort();
    assert_eq!(indexes, ["go-tools+gen.scip", "go.scip"]);
    let args = std::fs::read_to_string(&log).unwrap();
    let scanned: Vec<&str> = args.lines().filter(|a| a.ends_with(".go")).collect();
    let in_scope = [
        "cmd/app/main.go",
        "shapes/shapes.go",
        "shapes/shapes_test.go",
        "tools/gen/main.go",
    ];
    assert_eq!(scanned, [in_scope, in_scope].concat());

    let probe = repo.join("target/probe.sql");
    std::fs::write(
        &probe,
        "SELECT path, module, kind, qualified_name, start_line, end_line, is_test, \
                coalesce(visibility, '-'), coalesce(parameters::VARCHAR, '-'), \
                coalesce(member_of, '-') \
         FROM go.units ORDER BY unit_id;\n",
    )
    .unwrap();
    let units = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    let cells: Vec<Vec<&str>> = units
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    let shapes = "example.com/shapes";
    let n = "false";
    assert_eq!(
        cells,
        [
            vec![
                "cmd/app/main.go",
                shapes,
                "file",
                "main",
                "1",
                "8",
                n,
                "-",
                "-",
                "-"
            ],
            vec![
                "cmd/app/main.go",
                shapes,
                "function",
                "main",
                "5",
                "7",
                n,
                "private",
                "0",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "file",
                "shapes",
                "1",
                "29",
                n,
                "-",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "constant",
                "Small",
                "5",
                "5",
                n,
                "pub",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "constant",
                "Large",
                "6",
                "6",
                n,
                "pub",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "type",
                "Shape",
                "10",
                "12",
                n,
                "pub",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "method",
                "Shape.Area",
                "11",
                "11",
                n,
                "-",
                "1",
                "interface_member"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "type",
                "Circle",
                "14",
                "16",
                n,
                "pub",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "method",
                "Circle.Area",
                "18",
                "20",
                n,
                "pub",
                "1",
                "member"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "method",
                "Circle.scale",
                "22",
                "24",
                n,
                "private",
                "3",
                "member"
            ],
            vec![
                "shapes/shapes.go",
                shapes,
                "function",
                "Describe",
                "26",
                "28",
                n,
                "pub",
                "1",
                "-"
            ],
            vec![
                "shapes/shapes_test.go",
                shapes,
                "file",
                "shapes",
                "1",
                "10",
                "true",
                "-",
                "-",
                "-"
            ],
            vec![
                "shapes/shapes_test.go",
                shapes,
                "function",
                "TestArea",
                "5",
                "9",
                "true",
                "pub",
                "1",
                "-"
            ],
            vec![
                "tools/gen/main.go",
                "example.com/gen",
                "file",
                "main",
                "1",
                "20",
                n,
                "-",
                "-",
                "-"
            ],
            vec![
                "tools/gen/main.go",
                "example.com/gen",
                "function",
                "main",
                "3",
                "3",
                n,
                "private",
                "0",
                "-"
            ],
            vec![
                "tools/gen/main.go",
                "example.com/gen",
                "function",
                "grade",
                "5",
                "19",
                n,
                "private",
                "1",
                "-"
            ],
        ]
    );

    // What runs a Go unit without a reference: package main's files are roots, its
    // main an entry point.
    let structural = rows(&code_db(&[
        "query",
        "unreferenced",
        "include_structural=true",
    ]));
    let mut entries: Vec<(&str, &str, &str, &str)> = structural
        .iter()
        .filter(|r| r[7] == "root" || r[7] == "entry_point")
        .map(|r| (&r[0][..], &r[2][..], &r[3][..], &r[7][..]))
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        [
            ("cmd/app/main.go", "file", "main", "root"),
            ("cmd/app/main.go", "function", "main", "entry_point"),
            ("tools/gen/main.go", "file", "main", "root"),
            ("tools/gen/main.go", "function", "main", "entry_point"),
        ]
    );

    // Complexity in Go's constructs and words: two cases (a switch is not
    // exhaustive, the default counts none), `&&`, the for and the if inside it.
    let complexity = code_db(&["query", "complexity"]);
    let header: Vec<String> = complexity
        .lines()
        .next()
        .unwrap_or_default()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_owned())
        .collect();
    let grade = rows(&complexity)
        .into_iter()
        .find(|r| r[3] == "grade")
        .expect("grade's row");
    let cell = |name: &str| {
        let i = header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("no column {name}: {header:?}"));
        grade[i].clone()
    };
    for (name, value) in [
        ("cyclomatic", "6"),
        ("decisions", "5"),
        ("max_nesting", "2"),
        ("switch_count", "1"),
        ("case_count", "2"),
        ("and_count", "1"),
        ("for_count", "1"),
        ("if_count", "1"),
    ] {
        assert_eq!(cell(name), value, "{name}");
    }
    // Only Go's own constructs are columns: none of Rust's match, arm, guard or `?`.
    for absent in ["match_count", "arm_count", "guard_count", "try_count"] {
        assert!(!header.iter().any(|h| h == absent), "{absent}: {header:?}");
    }

    // A method belongs to its receiver's type, wherever the method is.
    std::fs::write(
        &probe,
        "SELECT u.qualified_name, t.qualified_name FROM go.unit_owner_types o \
         JOIN go.units u USING (unit_id) JOIN go.units t ON t.unit_id = o.type_unit_id ORDER BY ALL;\n",
    )
    .unwrap();
    let owners = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    assert_eq!(
        owners
            .iter()
            .map(|r| (&r[0][..], &r[1][..]))
            .collect::<Vec<_>>(),
        [("Circle.Area", "Circle"), ("Circle.scale", "Circle")]
    );

    // scip-go's implementation relationships: Circle implements Shape, and its
    // Area implements Shape's.
    std::fs::write(
        &probe,
        "SELECT t.qualified_name, i.qualified_name FROM go.type_interfaces x \
         JOIN go.units t ON t.unit_id = x.type_unit_id JOIN go.units i ON i.unit_id = x.interface_unit_id \
         UNION ALL \
         SELECT u.qualified_name, i.qualified_name FROM go.member_implementations m \
         JOIN go.units u USING (unit_id) JOIN go.units i ON i.unit_id = m.interface_member_unit_id \
         ORDER BY ALL;\n",
    )
    .unwrap();
    let implementations = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    assert_eq!(
        implementations
            .iter()
            .map(|r| (&r[0][..], &r[1][..]))
            .collect::<Vec<_>>(),
        [("Circle", "Shape"), ("Circle.Area", "Shape.Area")]
    );

    // A struct's fields are its parts.
    std::fs::write(
        &probe,
        "SELECT t.qualified_name, p.name, p.part FROM go.type_parts p \
         JOIN go.units t ON t.unit_id = p.type_unit_id ORDER BY ALL;\n",
    )
    .unwrap();
    let parts = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    assert_eq!(
        parts
            .iter()
            .map(|r| (&r[0][..], &r[1][..], &r[2][..]))
            .collect::<Vec<_>>(),
        [("Circle", "R", "field")]
    );

    // References across files and packages become unit edges.
    std::fs::write(
        &probe,
        "SELECT s.qualified_name, d.qualified_name FROM unit_edges e \
         JOIN go.units s ON s.unit_id = e.src_unit_id JOIN go.units d ON d.unit_id = e.dst_unit_id \
         WHERE s.kind <> 'file' AND NOT e.is_self ORDER BY ALL;\n",
    )
    .unwrap();
    let edges = rows(&code_db(&["sql", probe.to_str().unwrap()]));
    let pairs: Vec<(&str, &str)> = edges.iter().map(|r| (&r[0][..], &r[1][..])).collect();
    assert_eq!(
        pairs,
        [
            ("Circle.Area", "Circle"),
            ("Circle.scale", "Circle"),
            ("Describe", "Shape"),
            ("Describe", "Shape.Area"),
            ("TestArea", "Circle"),
            ("TestArea", "Circle.Area"),
            ("main", "Circle"),
            ("main", "Describe"),
        ]
    );

    // The queries, their parameters and their columns use Go's names: a Go
    // module where the views say package, a Go package where they say namespace.
    let graph = code_db(&["query", "package_graph"]);
    let header = graph.lines().next().unwrap_or_default();
    assert!(
        header.contains("src_package") && header.contains("src_module"),
        "{header}"
    );
    assert!(
        !header.contains("namespace") && !header.contains("crate"),
        "{header}"
    );
    // Namespaces are Go packages (directories): the files of a package are one
    // namespace, and the one edge left is between the module's two packages.
    let edges = rows(&graph);
    assert_eq!(
        edges
            .iter()
            .map(|r| (&r[0][..], &r[1][..]))
            .collect::<Vec<_>>(),
        [("cmd/app", "shapes")]
    );
    let modules = rows(&code_db(&["query", "coupling", "level=module"]));
    assert_eq!(
        modules
            .iter()
            .map(|r| (&r[0][..], &r[3][..]))
            .collect::<Vec<_>>(),
        [
            ("module", "example.com/gen"),
            ("module", "example.com/shapes")
        ]
    );
    // The package level (Go's namespace): the packages with an edge, and the one other
    // package each is used by or uses (fan_in_packages, fan_out_packages).
    let packages = rows(&code_db(&["query", "coupling", "level=package"]));
    assert_eq!(
        packages
            .iter()
            .map(|r| (&r[0][..], &r[2][..], &r[9][..], &r[13][..]))
            .collect::<Vec<_>>(),
        [
            ("package", "shapes", "1", "0"),
            ("package", "cmd/app", "0", "1"),
        ]
    );

    // A second language needs lang=NAME, which reads that language only.
    write(repo, "tools/build.rs", "fn main() {}\n");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add a Rust file"]);
    let output = Command::new(env!("CARGO_BIN_EXE_code-db"))
        .args(["query", repo.to_str().unwrap(), "size"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("several languages (go, rust)") && stderr.contains("lang="),
        "{stderr}"
    );
    let sizes = rows(&code_db(&["query", "size", "level=module", "lang=go"]));
    assert_eq!(sizes.len(), 2);
}

const WORK: &str = "go 1.22\n\nuse (\n\t./a\n\t./b\n)\n";
const WORK_A: &str = "\
package geom

func Area(side int) int {
	return side * side
}

type Shape interface {
	Size() int
}
";
const WORK_B: &str = "\
package main

import \"example.com/a/geom\"

type Box struct{}

func (Box) Size() int { return 1 }

func main() {
	var s geom.Shape = Box{}
	println(geom.Area(s.Size()))
}
";

/// The modules a tracked go.work uses are indexed together, so their
/// references to each other resolve; a module it does not use is indexed on
/// its own, outside the workspace.
#[test]
fn go_work_modules_index_together() {
    let scip_go = std::env::var("SCIP_GO").unwrap_or_else(|_| "scip-go".to_owned());
    if Command::new(&scip_go).arg("--version").output().is_err() {
        eprintln!("SKIPPED: {scip_go} not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let repo = repo.as_path();
    git(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
    write(repo, ".gitignore", "target/\n");
    write(repo, "go.work", WORK);
    write(repo, "a/go.mod", "module example.com/a\n\ngo 1.22\n");
    write(repo, "a/geom/geom.go", WORK_A);
    write(
        repo,
        "b/go.mod",
        "module example.com/b\n\ngo 1.22\n\nrequire example.com/a v0.0.0\n",
    );
    write(repo, "b/app/main.go", WORK_B);
    write(repo, "c/go.mod", "module example.com/c\n\ngo 1.22\n");
    write(repo, "c/c.go", "package c\n\nfunc C() int { return 1 }\n");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add the workspace"]);
    let jscpd = tmp.path().join("jscpd");
    write(tmp.path(), "jscpd", FAKE_JSCPD);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&jscpd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = tmp.path().join("jscpd.log");
    let code_db = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_code-db"))
            .args(args.iter().take(1))
            .arg(repo)
            .args(args.iter().skip(1))
            .env("JSCPD", &jscpd)
            .env("FAKE_JSCPD_LOG", &log)
            .env("RUST_ANALYZER", "/nonexistent/rust-analyzer")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "code-db {args:?}: {stderr}");
        String::from_utf8(output.stdout).unwrap()
    };

    code_db(&["index"]);
    let mut indexes: Vec<String> = std::fs::read_dir(repo.join("target/code-db/scip"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    indexes.sort();
    assert_eq!(indexes, ["go-c.scip", "gowork.scip"]);

    let probe = repo.join("target/probe.sql");
    let query = |sql: &str| {
        std::fs::write(&probe, sql).unwrap();
        rows(&code_db(&["sql", probe.to_str().unwrap()]))
    };
    assert_eq!(
        query(
            "SELECT s.qualified_name, d.qualified_name FROM unit_edges e \
             JOIN go.units s ON s.unit_id = e.src_unit_id JOIN go.units d ON d.unit_id = e.dst_unit_id \
             WHERE s.path <> d.path ORDER BY ALL;\n"
        ),
        [["main", "Area"], ["main", "Shape"], ["main", "Shape.Size"]]
    );
    assert_eq!(
        query(
            "SELECT u.qualified_name, i.qualified_name FROM go.member_implementations m \
             JOIN go.units u USING (unit_id) JOIN go.units i ON i.unit_id = m.interface_member_unit_id;\n"
        ),
        [["Box.Size", "Shape.Size"]]
    );
    assert_eq!(
        query("SELECT module FROM go.units WHERE path = 'c/c.go' AND kind = 'function';\n"),
        [["example.com/c"]]
    );
}

/// A module below the repository root, with an internal and an external test package
/// and function literals in a package-level `var`: the tests stay `test` although the
/// test main `go test` generates (a document in Go's build cache) references them, the
/// literals' parameters are no units, and the external test is a package of its own, so
/// it makes no cycle with a package that imports the one it tests.
#[test]
fn go_tests_literals_and_external_test_packages() {
    let scip_go = std::env::var("SCIP_GO").unwrap_or_else(|_| "scip-go".to_owned());
    if Command::new(&scip_go).arg("--version").output().is_err() {
        eprintln!("SKIPPED: {scip_go} not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let repo = repo.as_path();
    git(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
    write(repo, ".gitignore", "target/\n");
    write(repo, "sub/go.mod", "module example.com/m\n\ngo 1.22\n");
    write(
        repo,
        "sub/a/a.go",
        "package a\n\nfunc A() int { return 1 }\n",
    );
    write(
        repo,
        "sub/a/hooks.go",
        "package a\n\nfunc apply(f func(*int)) func(*int) { return f }\n\n\
         var hooks = []func(*int){\n\tapply(func(n *int) { *n = 1 }),\n\tapply(func(n *int) { *n = 2 }),\n}\n",
    );
    write(
        repo,
        "sub/a/in_test.go",
        "package a\n\nimport \"testing\"\n\nfunc TestIn(t *testing.T) { _ = A() + len(hooks) }\n",
    );
    write(
        repo,
        "sub/a/a_test.go",
        "// The external tests.\npackage a_test\n\nimport (\n\t\"testing\"\n\n\t\"example.com/m/a\"\n\t\"example.com/m/b\"\n)\n\n\
         func TestA(t *testing.T) { _ = a.A() + b.B() }\n",
    );
    write(
        repo,
        "sub/b/b.go",
        "package b\n\nimport \"example.com/m/a\"\n\nfunc B() int { return a.A() }\n",
    );
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "Add the module"]);
    let jscpd = tmp.path().join("jscpd");
    write(tmp.path(), "jscpd", FAKE_JSCPD);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&jscpd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = tmp.path().join("jscpd.log");
    let code_db = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_code-db"))
            .args(args.iter().take(1))
            .arg(repo)
            .args(args.iter().skip(1))
            .env("JSCPD", &jscpd)
            .env("FAKE_JSCPD_LOG", &log)
            .env("RUST_ANALYZER", "/nonexistent/rust-analyzer")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "code-db {args:?}: {stderr}");
        String::from_utf8(output.stdout).unwrap()
    };
    code_db(&["index"]);
    let probe = repo.join("target/probe.sql");
    let query = |sql: &str| {
        std::fs::write(&probe, sql).unwrap();
        rows(&code_db(&["sql", probe.to_str().unwrap()]))
    };

    assert_eq!(
        query(
            "SELECT u.path, u.kind, u.qualified_name, r.status FROM go.units u \
             JOIN go.unit_reference_status r USING (unit_id) \
             WHERE u.path IN ('sub/a/hooks.go', 'sub/a/a_test.go', 'sub/a/in_test.go') \
             AND u.kind <> 'file' ORDER BY ALL;\n"
        ),
        [
            ["sub/a/a_test.go", "function", "TestA", "test"],
            ["sub/a/hooks.go", "constant", "hooks", "referenced"],
            ["sub/a/hooks.go", "function", "apply", "referenced"],
            ["sub/a/in_test.go", "function", "TestIn", "test"],
        ]
    );
    // Documents outside the repository start with ../ once their path is resolved.
    assert_eq!(
        query(
            "SELECT count(*) > 0, bool_and(starts_with(document, '../')) FROM scip_occurrences \
             WHERE file_id IS NULL;\n"
        ),
        [["true", "true"]]
    );
    assert_eq!(
        query(
            "SELECT path, package FROM go.files JOIN go.file_packages USING (file_id) \
             WHERE path LIKE 'sub/a/%' ORDER BY path;\n"
        ),
        [
            ["sub/a/a.go", "sub/a"],
            ["sub/a/a_test.go", "sub/a_test"],
            ["sub/a/hooks.go", "sub/a"],
            ["sub/a/in_test.go", "sub/a"],
        ]
    );
    assert!(rows(&code_db(&["query", "package_cycles"])).is_empty());
    let graph = rows(&code_db(&["query", "package_graph"]));
    assert_eq!(
        graph
            .iter()
            .map(|r| (&r[0][..], &r[1][..], &r[8][..]))
            .collect::<Vec<_>>(),
        [
            ("sub/a_test", "sub/a", "true"),
            ("sub/a_test", "sub/b", "false"),
            ("sub/b", "sub/a", "false"),
        ]
    );
}
