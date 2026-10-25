//! `changed_blobs` against the `git` CLI on a fixture repository.
//!
//! The fixture is built with the `git` CLI under a temporary directory with
//! fixed names and dates and no user or system configuration (as in
//! `cli_parity.rs`). The expected rows come from `git log --raw --no-abbrev
//! -M` (old/new ids and modes), with git's binary rule (a NUL byte in the
//! first 8000 bytes; the fixture sets no attributes) applied to `git
//! cat-file -p` of each side, and every written file is compared with `git
//! cat-file -p` of its blob.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::process::Command;

use git_history_core::{self as history, BlobWriteStats, ChangedBlobRow, Range};

struct Fixture {
    dir: tempfile::TempDir,
    clock: u32,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Fixture {
            dir: tempfile::tempdir().expect("temp dir"),
            clock: 0,
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", self.path())
            .env("GIT_AUTHOR_NAME", "Fixture Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.invalid");
        command
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = self.command(args).output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let path = self.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn symlink(&self, target: &str, path: &str) {
        let path = self.path().join(path);
        let _ = fs::remove_file(&path);
        std::os::unix::fs::symlink(target, path).unwrap();
    }

    fn chmod(&self, path: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(self.path().join(path), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn commit(&mut self, message: &str) -> String {
        self.clock += 1;
        let date = format!("@{} +0900", 1_700_000_000 + 3600 * self.clock);
        self.git(&["add", "-A"]);
        let status = self
            .command(&["commit", "-q", "--allow-empty", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(status.success());
        self.head()
    }

    fn merge(&mut self, branch: &str, message: &str) -> String {
        self.clock += 1;
        let date = format!("@{} +0900", 1_700_000_000 + 3600 * self.clock);
        let status = self
            .command(&["merge", "-q", "--no-ff", "-m", message, branch])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(status.success());
        self.head()
    }

    fn head(&self) -> String {
        String::from_utf8(self.git(&["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_string()
    }

    fn cat(&self, id: &str) -> Vec<u8> {
        self.git(&["cat-file", "-p", id])
    }
}

fn lines(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|i| format!("line {i}\n")).collect()
}

struct Shas {
    root: String,
    shared: String,
    deletion: String,
    pure_rename: String,
    edited_rename: String,
    binary: String,
    type_change: String,
    mode_only: String,
    merge: String,
}

fn build() -> (Fixture, Shas) {
    let mut f = Fixture::new();
    f.write("a.rs", lines(1..=10));
    f.write("doc.txt", "alpha\n");
    f.write("bin.dat", [0u8, 1, 2, 3, 0, 5]);
    f.write("flip.rs", "fn text() {}\n");
    f.write("same.rs", "fn same() {}\n");
    f.write("run.sh", "#!/bin/sh\necho run\n");
    f.chmod("run.sh", 0o755);
    f.write("Makefile", "all:\n");
    f.symlink("a.rs", "link");
    f.symlink("doc.txt", "back");
    let root = f.commit("Root commit");

    // A modification, and a new file sharing a blob with same.rs.
    f.write("a.rs", lines(1..=10).replace("line 2\n", "line two\n"));
    f.write("copy.rs", "fn same() {}\n");
    f.write("Makefile", "all:\n\ttrue\n");
    let shared = f.commit("Edit a.rs, add copy.rs");

    f.git(&["rm", "-q", "doc.txt"]);
    let deletion = f.commit("Delete doc.txt");

    f.git(&["mv", "same.rs", "moved.rs"]);
    let pure_rename = f.commit("Move same.rs");

    fs::create_dir_all(f.path().join("src")).unwrap();
    f.git(&["mv", "a.rs", "src/a.rs"]);
    f.write(
        "src/a.rs",
        lines(1..=10)
            .replace("line 2\n", "line two\n")
            .replace("line 7\n", "line seven\n"),
    );
    f.symlink("src/a.rs", "link");
    let edited_rename = f.commit("Move and edit a.rs, retarget link");

    // A binary change, and a text file turned binary.
    f.write("bin.dat", [0u8, 9, 9, 9]);
    f.write("flip.rs", [b'x', 0, b'y']);
    let binary = f.commit("Change binaries");

    // A file turned into a symlink and a symlink turned into a file (T).
    f.write("kind.rs", "fn kind() {}\n");
    f.commit("Add kind.rs");
    f.symlink("src/a.rs", "kind.rs");
    fs::remove_file(f.path().join("back")).unwrap();
    f.write("back", "now a file\n");
    let type_change = f.commit("Swap kinds");

    // Only the executable bit changes, and an executable file changes content.
    f.chmod("run.sh", 0o644);
    f.write("tool.sh", "#!/bin/sh\n");
    f.chmod("tool.sh", 0o755);
    f.commit("Add tool.sh, clear run.sh's executable bit");
    let mode_only = f.head();
    f.write("tool.sh", "#!/bin/sh\necho tool\n");
    f.commit("Edit tool.sh");

    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("side.rs", "fn side() {}\n");
    f.commit("Side work");
    f.git(&["checkout", "-q", "main"]);
    f.write("main.rs", "fn main() {}\n");
    f.commit("Main work");
    let merge = f.merge("side", "Merge side");

    let shas = Shas {
        root,
        shared,
        deletion,
        pure_rename,
        edited_rename,
        binary,
        type_change,
        mode_only,
        merge,
    };
    (f, shas)
}

fn revisions(range: &Range) -> String {
    match &range.since {
        Some(since) => format!("{since}..HEAD"),
        None => "HEAD".into(),
    }
}

fn merge_flag(range: &Range) -> &'static str {
    if range.include_merges {
        "--diff-merges=first-parent"
    } else {
        "--diff-merges=off"
    }
}

/// A row without its `file` column, which the test checks separately.
type Key = (String, String, Option<String>, &'static str, String, String);

fn key(row: &ChangedBlobRow) -> Key {
    (
        row.sha.clone(),
        row.path.clone(),
        row.old_path.clone(),
        row.side,
        row.blob_id.clone(),
        row.mode.to_string(),
    )
}

fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next().unwrap();
    name.rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty())
}

/// The rows `changed_blobs` should return, from `git log --raw --no-abbrev -M`.
fn cli_rows(f: &Fixture, range: &Range, extensions: Option<&[&str]>) -> BTreeSet<Key> {
    let out = f.git(&[
        "log",
        &revisions(range),
        "--raw",
        "-M",
        "--no-abbrev",
        "--no-color",
        merge_flag(range),
        "--format=%x01%H",
    ]);
    let null = "0".repeat(40);
    let regular = |mode: &str| mode == "100644" || mode == "100755";
    let mut rows = BTreeSet::new();
    let mut sha = String::new();
    for line in String::from_utf8(out).unwrap().lines() {
        if let Some(rest) = line.strip_prefix('\x01') {
            sha = rest.to_string();
            continue;
        }
        let Some(raw) = line.strip_prefix(':') else {
            continue;
        };
        let (meta, paths) = raw.split_once('\t').unwrap();
        let fields: Vec<&str> = meta.split(' ').collect();
        let (old_mode, new_mode, old_id, new_id, status) =
            (fields[0], fields[1], fields[2], fields[3], &fields[4][..1]);
        let paths: Vec<&str> = paths.split('\t').collect();
        let (old_path, path) = if paths.len() == 2 {
            (paths[0], paths[1])
        } else {
            (paths[0], paths[0])
        };
        if old_id == new_id && regular(old_mode) && regular(new_mode) {
            continue;
        }
        let renamed = (status == "R").then(|| old_path.to_string());
        for (side, mode, id, side_path) in [
            ("old", old_mode, old_id, old_path),
            ("new", new_mode, new_id, path),
        ] {
            if id == null || !regular(mode) {
                continue;
            }
            if let Some(list) = extensions
                && !extension(side_path).is_some_and(|e| list.contains(&e))
            {
                continue;
            }
            let content = f.cat(id);
            if content[..content.len().min(8000)].contains(&0) {
                continue;
            }
            rows.insert((
                sha.clone(),
                path.to_string(),
                renamed.clone(),
                side,
                id.to_string(),
                mode.to_string(),
            ));
        }
    }
    rows
}

fn run(
    f: &Fixture,
    range: &Range,
    dir: &Path,
    extensions: Option<&[&str]>,
) -> (Vec<ChangedBlobRow>, BlobWriteStats) {
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    let extensions: Option<Vec<String>> =
        extensions.map(|list| list.iter().map(|e| e.to_string()).collect());
    history::changed_blobs(&repo, range, dir, extensions.as_deref()).unwrap()
}

/// Every row's file is `<dir>/<blob_id>[.<ext>]` with the blob's exact bytes.
fn assert_files(f: &Fixture, dir: &Path, rows: &[ChangedBlobRow]) {
    for row in rows {
        let side_path = match (row.side, &row.old_path) {
            ("old", Some(old)) => old.as_str(),
            _ => row.path.as_str(),
        };
        let name = match extension(side_path) {
            Some(ext) => format!("{}.{ext}", row.blob_id),
            None => row.blob_id.clone(),
        };
        assert_eq!(row.file, dir.join(name).to_str().unwrap(), "{row:?}");
        assert_eq!(fs::read(&row.file).unwrap(), f.cat(&row.blob_id), "{row:?}");
    }
}

/// The files in `dir`, without temporaries (there should be none).
fn listing(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn ranges(shas: &Shas) -> Vec<Range> {
    let mut out = Vec::new();
    for since in [None, Some(shas.deletion.clone())] {
        for include_merges in [false, true] {
            out.push(Range {
                since: since.clone(),
                include_merges,
            });
        }
    }
    out
}

#[test]
fn rows_and_files_match_the_git_cli() {
    let (f, shas) = build();
    for range in ranges(&shas) {
        for extensions in [None, Some(&["rs"][..]), Some(&["rs", "sh"][..])] {
            let out = tempfile::tempdir().unwrap();
            let dir = out.path().join("blobs");
            let (rows, stats) = run(&f, &range, &dir, extensions);
            let got: BTreeSet<Key> = rows.iter().map(key).collect();
            assert_eq!(got.len(), rows.len(), "duplicate rows for {range:?}");
            assert_eq!(
                got,
                cli_rows(&f, &range, extensions),
                "{range:?} {extensions:?}"
            );
            assert_files(&f, &dir, &rows);
            let files: BTreeSet<&str> = rows.iter().map(|r| r.file.as_str()).collect();
            assert_eq!(stats.written, files.len());
            assert_eq!(stats.skipped, 0);
            let bytes: u64 = files.iter().map(|p| fs::metadata(p).unwrap().len()).sum();
            assert_eq!(stats.bytes_written, bytes);
            assert_eq!(listing(&dir).len(), files.len(), "only blob files in dir");
        }
    }
}

/// Rows follow `file_changes`'s order (git log order, then its order of
/// changes), `old` before `new`; their `(sha, path, old_path)` set is the
/// CLI's, and the changes of `file_changes` without rows are exactly those
/// with no regular, non-binary side whose content changed.
#[test]
fn rows_follow_file_changes() {
    let (f, shas) = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    for range in ranges(&shas) {
        let out = tempfile::tempdir().unwrap();
        let (rows, _) = run(&f, &range, out.path(), None);
        let changes = history::file_changes(&repo, &range).unwrap();
        let position: HashMap<_, _> = changes
            .iter()
            .enumerate()
            .map(|(i, c)| ((c.sha.clone(), c.path.clone(), c.old_path.clone()), i))
            .collect();
        let order: Vec<(usize, u8)> = rows
            .iter()
            .map(|r| {
                let at = position[&(r.sha.clone(), r.path.clone(), r.old_path.clone())];
                (at, u8::from(r.side == "new"))
            })
            .collect();
        assert!(order.is_sorted(), "{range:?}: {order:?}");

        let with_rows: BTreeSet<_> = rows
            .iter()
            .map(|r| (r.sha.clone(), r.path.clone(), r.old_path.clone()))
            .collect();
        let cli: BTreeSet<_> = cli_rows(&f, &range, None)
            .into_iter()
            .map(|(sha, path, old_path, ..)| (sha, path, old_path))
            .collect();
        assert_eq!(with_rows, cli, "{range:?}");
        let without: BTreeSet<(String, String)> = changes
            .iter()
            .filter(|c| !with_rows.contains(&(c.sha.clone(), c.path.clone(), c.old_path.clone())))
            .map(|c| (c.sha.clone(), c.path.clone()))
            .collect();
        assert_eq!(without.len() + with_rows.len(), changes.len());
        if range == Range::default() {
            let expected: BTreeSet<(String, String)> = [
                (&shas.root, "link"),
                (&shas.root, "back"),
                (&shas.root, "bin.dat"),
                (&shas.pure_rename, "moved.rs"),
                (&shas.edited_rename, "link"),
                (&shas.binary, "bin.dat"),
                (&shas.mode_only, "run.sh"),
            ]
            .into_iter()
            .map(|(sha, path)| (sha.clone(), path.to_string()))
            .collect();
            assert_eq!(without, expected);
        }
    }
}

#[test]
fn each_case_has_the_documented_rows() {
    let (f, shas) = build();
    let out = tempfile::tempdir().unwrap();
    let range = Range::default();
    let (rows, _) = run(&f, &range, out.path(), None);
    let of = |sha: &str, path: &str| -> Vec<(&'static str, String)> {
        rows.iter()
            .filter(|r| r.sha == sha && r.path == path)
            .map(|r| (r.side, r.mode.to_string()))
            .collect()
    };
    let new = |mode: &str| vec![("new", mode.to_string())];
    let old = |mode: &str| vec![("old", mode.to_string())];
    let both = vec![("old", "100644".into()), ("new", "100644".into())];

    // Additions: new only; symlinks and binary files: nothing.
    assert_eq!(of(&shas.root, "a.rs"), new("100644"));
    assert_eq!(of(&shas.root, "run.sh"), new("100755"));
    assert_eq!(of(&shas.root, "Makefile"), new("100644"));
    assert!(of(&shas.root, "link").is_empty());
    assert!(of(&shas.root, "bin.dat").is_empty());
    // A modification: both sides.
    assert_eq!(of(&shas.shared, "a.rs"), both);
    // A deletion: old only.
    assert_eq!(of(&shas.deletion, "doc.txt"), old("100644"));
    // A rename without content change: nothing.
    assert!(of(&shas.pure_rename, "moved.rs").is_empty());
    // A rename with change: both, old_path set; a retargeted symlink: nothing.
    assert_eq!(of(&shas.edited_rename, "src/a.rs"), both);
    assert!(
        rows.iter()
            .filter(|r| r.sha == shas.edited_rename && r.path == "src/a.rs")
            .all(|r| r.old_path.as_deref() == Some("a.rs"))
    );
    assert!(of(&shas.edited_rename, "link").is_empty());
    // Binary on both sides: nothing; text turned binary: the text side only.
    assert!(of(&shas.binary, "bin.dat").is_empty());
    assert_eq!(of(&shas.binary, "flip.rs"), old("100644"));
    // Type changes: the regular side only.
    assert_eq!(of(&shas.type_change, "kind.rs"), old("100644"));
    assert_eq!(of(&shas.type_change, "back"), new("100644"));
    // An executable bit change alone: nothing.
    assert!(of(&shas.mode_only, "run.sh").is_empty());
    assert_eq!(of(&shas.mode_only, "tool.sh"), new("100755"));
    // A merge: no rows without include_merges.
    assert!(rows.iter().all(|r| r.sha != shas.merge));
    let merges = Range {
        since: None,
        include_merges: true,
    };
    let (rows, _) = run(&f, &merges, out.path(), None);
    let merge_rows: Vec<_> = rows.iter().filter(|r| r.sha == shas.merge).collect();
    assert_eq!(merge_rows.len(), 1);
    assert_eq!(
        (merge_rows[0].path.as_str(), merge_rows[0].side),
        ("side.rs", "new")
    );

    // `since` excludes the commits it reaches.
    let since = Range {
        since: Some(shas.deletion.clone()),
        include_merges: false,
    };
    let (rows, _) = run(&f, &since, out.path(), None);
    assert!(
        rows.iter()
            .all(|r| ![&shas.root, &shas.shared, &shas.deletion].contains(&&r.sha))
    );
    assert!(rows.iter().any(|r| r.sha == shas.edited_rename));
}

#[test]
fn a_shared_blob_is_written_once() {
    let (f, shas) = build();
    let out = tempfile::tempdir().unwrap();
    let (rows, stats) = run(&f, &Range::default(), out.path(), None);
    let same: Vec<_> = rows
        .iter()
        .filter(|r| {
            (r.sha == shas.root && r.path == "same.rs")
                || (r.sha == shas.shared && r.path == "copy.rs")
        })
        .collect();
    assert_eq!(same.len(), 2);
    assert_eq!(same[0].blob_id, same[1].blob_id);
    assert_eq!(same[0].file, same[1].file);
    let files: BTreeSet<_> = rows.iter().map(|r| r.file.clone()).collect();
    assert!(files.len() < rows.len());
    assert_eq!(stats.written, files.len());
}

#[test]
fn extensions_are_judged_per_side() {
    let (f, shas) = build();
    let out = tempfile::tempdir().unwrap();
    // `back` has no extension: never matched; Makefile neither.
    let (rows, _) = run(&f, &Range::default(), out.path(), Some(&["rs"]));
    assert!(rows.iter().all(|r| {
        let side_path = match (r.side, &r.old_path) {
            ("old", Some(old)) => old,
            _ => &r.path,
        };
        side_path.ends_with(".rs")
    }));
    assert!(rows.iter().any(|r| r.sha == shas.edited_rename));
    assert!(
        rows.iter()
            .all(|r| r.path != "back" && r.path != "Makefile")
    );
    // An empty list matches nothing.
    let (rows, stats) = run(&f, &Range::default(), out.path(), Some(&[]));
    assert!(rows.is_empty());
    assert_eq!(stats, BlobWriteStats::default());
}

#[test]
fn existing_files_are_skipped_and_wrong_sizes_replaced() {
    let (f, _) = build();
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("nested/blobs");
    let (rows, first) = run(&f, &Range::default(), &dir, None);
    assert!(first.written > 0);
    let before = listing(&dir);

    // One file truncated, one replaced by other bytes of the same size.
    let truncated = rows[0].file.clone();
    fs::write(&truncated, b"").unwrap();
    let same_size = rows
        .iter()
        .map(|r| r.file.clone())
        .find(|p| *p != truncated)
        .unwrap();
    let len = fs::metadata(&same_size).unwrap().len() as usize;
    fs::write(&same_size, vec![b'?'; len]).unwrap();

    let (again, second) = run(&f, &Range::default(), &dir, None);
    assert_eq!(again, rows, "a second run returns the same rows");
    assert_eq!(second.written, 1, "only the truncated file is rewritten");
    assert_eq!(second.skipped, first.written - 1);
    assert_eq!(listing(&dir), before);
    assert_eq!(
        fs::read(&truncated).unwrap(),
        f.cat(&rows[0].blob_id),
        "rewritten with the blob"
    );
    // Content equality is implied by the id, so a same-size file is trusted.
    assert_eq!(fs::read(&same_size).unwrap(), vec![b'?'; len]);

    let (_, third) = run(&f, &Range::default(), &dir, None);
    assert_eq!(third.written, 0);
    assert_eq!(third.skipped, first.written);
}

#[test]
fn a_relative_dir_is_an_error() {
    let (f, _) = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    let error =
        history::changed_blobs(&repo, &Range::default(), Path::new("blobs"), None).unwrap_err();
    assert!(error.to_string().contains("absolute"), "{error}");
}
