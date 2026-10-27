//! `tree` against `git ls-tree -r -z` on a fixture repository.
//!
//! The fixture is built with the `git` CLI under a temporary directory with
//! fixed names and dates and no user or system configuration: two commits on
//! `main` (the second deletes, renames and modifies files), a branch and a
//! lightweight and an annotated tag on the first. Every revision form is
//! compared with the CLI's listing of the same revision.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use git_history_core::{self as history, TreeRow};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git_at(&self, args: &[&str], date: &str) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".no-xdg"))
            .env("GIT_AUTHOR_NAME", "Fixture Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_at(args, "@1700000000 +0900")
    }

    fn rev_parse(&self, rev: &str) -> String {
        String::from_utf8(self.git(&["rev-parse", rev]))
            .unwrap()
            .trim()
            .to_string()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn open(&self) -> history::Repository {
        history::open(self.path().to_str().unwrap()).unwrap()
    }
}

const GITLINK: &str = "1111111111111111111111111111111111111111";

fn build() -> Fixture {
    let f = Fixture {
        dir: tempfile::tempdir().expect("temp dir"),
    };
    f.git(&["init", "-q", "-b", "main"]);
    f.write("a.txt", "a\n");
    f.write("src/lib.rs", "pub fn f() {}\n");
    f.write("src/deep/x.rs", "fn x() {}\n");
    // Names around a directory `s` that sort differently in git's tree order
    // (`s/` as a directory) than as bare names.
    f.write("s.txt", "dot\n");
    f.write("s-x.txt", "dash\n");
    f.write("s/y.txt", "inside\n");
    f.write("名前.txt", "非ASCII\n");
    f.write("with space.txt", "space\n");
    f.write("run.sh", "#!/bin/sh\n");
    let run = f.path().join("run.sh");
    fs::set_permissions(run, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("a.txt", f.path().join("link")).unwrap();
    f.write("old.txt", "deleted later\n");
    f.write(
        "move.txt",
        "renamed later\nwith enough lines\nto stay similar\n",
    );
    f.git(&["add", "-A"]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{GITLINK},vendor/sub"),
    ]);
    // An empty directory keeps the gitlink from reading as deleted to `add -A`.
    fs::create_dir_all(f.path().join("vendor/sub")).unwrap();
    f.git_at(&["commit", "-q", "-m", "First"], "@1700000000 +0900");
    f.git(&["branch", "feature"]);
    f.git(&["tag", "v1"]);
    f.git_at(
        &["tag", "-a", "-m", "Annotated", "v1a"],
        "@1700000000 +0900",
    );

    fs::remove_file(f.path().join("old.txt")).unwrap();
    fs::create_dir(f.path().join("moved")).unwrap();
    fs::rename(f.path().join("move.txt"), f.path().join("moved/move.txt")).unwrap();
    f.write("a.txt", "a changed\n");
    f.git(&["add", "-A"]);
    f.git_at(&["commit", "-q", "-m", "Second"], "@1700003600 +0900");
    f
}

/// The rows of `git ls-tree -r -z <rev>`, checking that each entry's type
/// matches its mode.
fn expected(f: &Fixture, rev: &str) -> Vec<TreeRow> {
    let output = f.git(&["ls-tree", "-r", "-z", rev]);
    output
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|record| {
            let record = String::from_utf8(record.to_vec()).unwrap();
            let (meta, path) = record.split_once('\t').unwrap();
            let fields: Vec<&str> = meta.split(' ').collect();
            let (mode, kind, id) = (fields[0], fields[1], fields[2]);
            let mode = match (mode, kind) {
                ("100644", "blob") => "100644",
                ("100755", "blob") => "100755",
                ("120000", "blob") => "120000",
                ("160000", "commit") => "160000",
                other => panic!("unexpected ls-tree entry {other:?}"),
            };
            TreeRow {
                path: path.to_string(),
                mode,
                blob_id: id.to_string(),
            }
        })
        .collect()
}

fn row<'a>(rows: &'a [TreeRow], path: &str) -> Option<&'a TreeRow> {
    rows.iter().find(|r| r.path == path)
}

#[test]
fn tree_matches_git_ls_tree_for_every_revision_form() {
    let f = build();
    let repo = f.open();
    let first = f.rev_parse("HEAD~1");
    let first_tree = f.rev_parse("HEAD~1^{tree}");
    let head_tree = f.rev_parse("HEAD^{tree}");
    let revs = [
        "HEAD",
        "main",
        "HEAD~1",
        "feature",
        "v1",
        "v1a",
        first.as_str(),
        &first[..10],
        first_tree.as_str(),
        head_tree.as_str(),
        "HEAD:src",
    ];
    for rev in revs {
        let rows = history::tree(&repo, rev).unwrap();
        assert_eq!(rows, expected(&f, rev), "rev {rev}");
        let mut sorted = rows.clone();
        sorted.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        assert_eq!(rows, sorted, "rev {rev} is sorted by path bytes");
        assert_eq!(rows, history::tree(&repo, rev).unwrap(), "rev {rev} twice");
    }
}

#[test]
fn tree_rows_cover_each_entry_kind_and_the_history() {
    let f = build();
    let repo = f.open();
    let head = history::tree(&repo, "HEAD").unwrap();
    let first = history::tree(&repo, "v1").unwrap();

    let blob = |rev: &str| f.rev_parse(rev);
    assert_eq!(row(&head, "run.sh").unwrap().mode, "100755");
    assert_eq!(row(&head, "src/deep/x.rs").unwrap().mode, "100644");
    let link = row(&head, "link").unwrap();
    assert_eq!(link.mode, "120000");
    // A symlink's blob holds its target path.
    assert_eq!(
        String::from_utf8(f.git(&["cat-file", "-p", &link.blob_id])).unwrap(),
        "a.txt"
    );
    let gitlink = row(&head, "vendor/sub").unwrap();
    assert_eq!(
        (gitlink.mode, gitlink.blob_id.as_str()),
        ("160000", GITLINK)
    );
    // No directories.
    for dir in ["src", "src/deep", "s", "vendor", "moved"] {
        assert!(row(&head, dir).is_none(), "{dir} is a directory");
    }

    // The deletion, the rename and the modification between the two commits.
    assert!(row(&first, "old.txt").is_some());
    assert!(row(&head, "old.txt").is_none());
    assert_eq!(
        row(&first, "move.txt").unwrap().blob_id,
        row(&head, "moved/move.txt").unwrap().blob_id
    );
    assert!(row(&head, "move.txt").is_none());
    assert_eq!(row(&first, "a.txt").unwrap().blob_id, blob("v1:a.txt"));
    assert_eq!(row(&head, "a.txt").unwrap().blob_id, blob("HEAD:a.txt"));
    assert_ne!(
        row(&first, "a.txt").unwrap().blob_id,
        row(&head, "a.txt").unwrap().blob_id
    );

    // A subtree's paths are relative to it.
    let src = history::tree(&repo, "HEAD:src").unwrap();
    let paths: Vec<&str> = src.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["deep/x.rs", "lib.rs"]);

    // Byte order: `s-x.txt` < `s.txt` < `s/y.txt`.
    let s: Vec<&str> = head
        .iter()
        .map(|r| r.path.as_str())
        .filter(|p| p.starts_with('s') && !p.starts_with("src"))
        .collect();
    assert_eq!(s, ["s-x.txt", "s.txt", "s/y.txt"]);
}

#[test]
fn an_unknown_or_non_tree_revision_is_an_error_naming_it() {
    let f = build();
    let repo = f.open();
    for rev in [
        "no-such-ref",
        "HEAD~5",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        let error = history::tree(&repo, rev).unwrap_err().to_string();
        assert!(error.contains(&format!("'{rev}'")), "{rev}: {error}");
    }
    // A blob id names no tree.
    let error = history::tree(&repo, "HEAD:a.txt").unwrap_err().to_string();
    assert!(error.contains("'HEAD:a.txt'"), "{error}");
}
