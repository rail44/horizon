//! The three functions' rows against the `git` CLI on a fixture repository.
//!
//! The fixture is built with the `git` CLI under a temporary directory with
//! fixed names and dates and no user or system configuration, so the test is
//! deterministic and offline. Expectations are parsed from `git log` output
//! the way `scripts/code-db/history.py` parses it.

use std::fs;
use std::path::Path;
use std::process::Command;

use git_history::history::{self, CommitRow, FileChangeRow, HunkRow, Range};

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

    /// Commit everything with the next fixed date; returns the commit id.
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
}

fn lines(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|i| format!("line {i}\n")).collect()
}

struct Shas {
    root: String,
    multi_hunk: String,
    deletion: String,
    pure_rename: String,
    edited_rename: String,
    binary: String,
    merge: String,
}

/// Several commits covering every case the functions distinguish.
fn build() -> (Fixture, Shas) {
    let mut f = Fixture::new();
    f.write("a.rs", lines(1..=10));
    f.write("doc.txt", "alpha\n");
    f.write("bin.dat", [0u8, 1, 2, 3, 0, 5]);
    let root = f.commit("Root commit");

    // Two separate hunks in a.rs, an insertion-only hunk in doc.txt.
    f.write(
        "a.rs",
        lines(1..=10)
            .replace("line 2\n", "line two\n")
            .replace("line 9\n", "line nine\n"),
    );
    f.write("doc.txt", "alpha\nbeta\ngamma\n");
    let multi_hunk = f.commit("First line\nwraps here  \n\nBody text");

    // A deletion-only hunk.
    f.write(
        "a.rs",
        lines(1..=10)
            .replace("line 2\n", "line two\n")
            .replace("line 9\n", "line nine\n")
            .replace("line 5\nline 6\n", ""),
    );
    let deletion = f.commit("Delete two lines");

    // A rename without content change.
    fs::create_dir_all(f.path().join("docs")).unwrap();
    f.git(&["mv", "doc.txt", "docs/doc.txt"]);
    let pure_rename = f.commit("Move doc");

    // A rename with a content change.
    fs::create_dir_all(f.path().join("src")).unwrap();
    f.git(&["mv", "a.rs", "src/a.rs"]);
    let moved = fs::read_to_string(f.path().join("src/a.rs"))
        .unwrap()
        .replace("line 7\n", "line seven\n");
    f.write("src/a.rs", moved);
    let edited_rename = f.commit("Move and edit a.rs");

    // A binary change.
    f.write("bin.dat", [0u8, 9, 9, 9]);
    let binary = f.commit("Change binary");

    // A file turned into a symlink (status T, printed as deletion plus creation).
    f.write("kind.txt", "regular\n");
    f.commit("Add kind.txt");
    fs::remove_file(f.path().join("kind.txt")).unwrap();
    std::os::unix::fs::symlink("src/a.rs", f.path().join("kind.txt")).unwrap();
    f.commit("Make kind.txt a symlink");

    // A side branch merged back with a merge commit.
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("side.rs", "fn side() {}\n");
    f.commit("Side work");
    f.git(&["checkout", "-q", "main"]);
    let edited = fs::read_to_string(f.path().join("src/a.rs"))
        .unwrap()
        .replace("line 1\n", "line one\n");
    f.write("src/a.rs", edited);
    f.commit("Main work");
    let merge = f.merge("side", "Merge side");

    let shas = Shas {
        root,
        multi_hunk,
        deletion,
        pure_rename,
        edited_rename,
        binary,
        merge,
    };
    (f, shas)
}

fn merge_flag(range: &Range) -> &'static str {
    if range.include_merges {
        "--diff-merges=first-parent"
    } else {
        "--diff-merges=off"
    }
}

fn revisions(range: &Range) -> String {
    match &range.since {
        Some(since) => format!("{since}..HEAD"),
        None => "HEAD".into(),
    }
}

fn cli_commits(f: &Fixture, range: &Range) -> Vec<CommitRow> {
    let out = f.git(&[
        "log",
        &revisions(range),
        "--format=%H%x1f%at%x1f%an%x1f%P%x1f%s",
    ]);
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\x1f').collect();
            let parent_count = fields[3].split_whitespace().count() as i32;
            CommitRow {
                sha: fields[0].into(),
                author_time: fields[1].parse().unwrap(),
                author_name: fields[2].into(),
                subject: fields[4].into(),
                parent_count,
                is_merge: parent_count > 1,
            }
        })
        .collect()
}

fn cli_hunks(f: &Fixture, range: &Range) -> Vec<HunkRow> {
    let out = f.git(&[
        "log",
        &revisions(range),
        "-p",
        "-U0",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        merge_flag(range),
        "--format=%x01%H",
    ]);
    let text = String::from_utf8(out).unwrap();
    let (mut sha, mut old_path, mut new_path) = (String::new(), None::<String>, None::<String>);
    let mut remaining = 0;
    let mut rows = Vec::new();
    let count = |value: Option<&str>| value.map_or(1, |v| v.parse().unwrap());
    for line in text.lines() {
        if remaining > 0 {
            if !line.starts_with('\\') {
                remaining -= 1;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('\x01') {
            sha = rest.into();
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old_path = (rest != "/dev/null").then(|| rest[2..].to_string());
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            new_path = (rest != "/dev/null").then(|| rest[2..].to_string());
        } else if let Some(rest) = line.strip_prefix("@@ -") {
            let header = rest.split(" @@").next().unwrap();
            let (old, new) = header.split_once(" +").unwrap();
            let mut old = old.split(',');
            let mut new = new.split(',');
            let (old_start, old_lines) = (old.next().unwrap().parse().unwrap(), count(old.next()));
            let (new_start, new_lines) = (new.next().unwrap().parse().unwrap(), count(new.next()));
            remaining = old_lines + new_lines;
            rows.push(HunkRow {
                sha: sha.clone(),
                path: new_path.clone().or(old_path.clone()).unwrap(),
                old_start,
                old_lines,
                new_start,
                new_lines,
            });
        } else if line.starts_with("diff --git ") {
            old_path = None;
            new_path = None;
        }
    }
    rows.sort();
    rows
}

fn cli_file_changes(f: &Fixture, range: &Range) -> Vec<FileChangeRow> {
    let out = f.git(&[
        "log",
        &revisions(range),
        "--raw",
        "--numstat",
        "-M",
        "--no-abbrev",
        "-z",
        "--no-color",
        merge_flag(range),
        "--format=%x01%H",
    ]);
    let text = String::from_utf8(out).unwrap();
    let mut rows = Vec::new();
    for record in text.split('\x01').skip(1) {
        let (sha, rest) = record.split_once('\0').unwrap_or((record, ""));
        let sha = sha.trim();
        let mut tokens = rest.split('\0');
        let mut entries = Vec::new();
        let mut stats = std::collections::HashMap::new();
        while let Some(token) = tokens.next() {
            let token = token.trim_start_matches('\n');
            if token.is_empty() {
                continue;
            }
            if let Some(raw) = token.strip_prefix(':') {
                let status = raw.split(' ').nth(4).unwrap()[..1].to_string();
                let mut path = tokens.next().unwrap().to_string();
                let mut old_path = None;
                if status == "R" || status == "C" {
                    old_path = Some(path);
                    path = tokens.next().unwrap().to_string();
                }
                entries.push((status, old_path, path));
            } else {
                let mut fields = token.splitn(3, '\t');
                let added = fields.next().unwrap().parse().ok();
                let deleted = fields.next().unwrap().parse().ok();
                let mut path = fields.next().unwrap().to_string();
                if path.is_empty() {
                    tokens.next();
                    path = tokens.next().unwrap().to_string();
                }
                stats.insert(path, (added, deleted));
            }
        }
        for (status, old_path, path) in entries {
            let (added, deleted) = stats.get(&path).copied().unwrap_or((None, None));
            rows.push(FileChangeRow {
                sha: sha.into(),
                path,
                old_path,
                status,
                added,
                deleted,
            });
        }
    }
    rows.sort();
    rows
}

fn sorted<T: Ord>(mut rows: Vec<T>) -> Vec<T> {
    rows.sort();
    rows
}

fn ranges(shas: &Shas) -> Vec<Range> {
    let mut ranges = Vec::new();
    for since in [None, Some(shas.pure_rename.clone())] {
        for include_merges in [false, true] {
            ranges.push(Range {
                since: since.clone(),
                include_merges,
            });
        }
    }
    ranges
}

#[test]
fn rows_equal_git_cli_output() {
    let (f, shas) = build();
    // Any directory inside the worktree resolves to the repository.
    let repo = history::open(f.path().join("src").to_str().unwrap()).unwrap();
    for range in ranges(&shas) {
        assert_eq!(
            history::commits(&repo, &range).unwrap(),
            cli_commits(&f, &range),
            "{range:?}"
        );
        assert_eq!(
            sorted(history::hunks(&repo, &range).unwrap()),
            cli_hunks(&f, &range),
            "{range:?}"
        );
        assert_eq!(
            sorted(history::file_changes(&repo, &range).unwrap()),
            cli_file_changes(&f, &range),
            "{range:?}"
        );
    }
}

#[test]
fn fixture_covers_each_case() {
    let (f, s) = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    let all = Range::default();
    let hunks = history::hunks(&repo, &all).unwrap();
    let hunks_of = |sha: &str, path: &str| -> Vec<[i32; 4]> {
        hunks
            .iter()
            .filter(|h| h.sha == sha && h.path == path)
            .map(|h| [h.old_start, h.old_lines, h.new_start, h.new_lines])
            .collect()
    };
    // The root commit diffs against the empty tree; binary files have no hunks.
    assert_eq!(hunks_of(&s.root, "a.rs"), [[0, 0, 1, 10]]);
    assert!(hunks_of(&s.root, "bin.dat").is_empty());
    assert_eq!(
        hunks_of(&s.multi_hunk, "a.rs"),
        [[2, 1, 2, 1], [9, 1, 9, 1]]
    );
    assert_eq!(hunks_of(&s.multi_hunk, "doc.txt"), [[1, 0, 2, 2]]);
    assert_eq!(hunks_of(&s.deletion, "a.rs"), [[5, 2, 4, 0]]);
    assert!(hunks_of(&s.pure_rename, "docs/doc.txt").is_empty());
    assert_eq!(hunks_of(&s.edited_rename, "src/a.rs"), [[5, 1, 5, 1]]);
    assert!(hunks.iter().all(|h| h.sha != s.merge));

    let changes = history::file_changes(&repo, &all).unwrap();
    let change = |sha: &str, path: &str| {
        changes
            .iter()
            .find(|c| c.sha == sha && c.path == path)
            .cloned()
            .unwrap()
    };
    let pure = change(&s.pure_rename, "docs/doc.txt");
    assert_eq!(
        (
            pure.status.as_str(),
            pure.old_path.as_deref(),
            pure.added,
            pure.deleted
        ),
        ("R", Some("doc.txt"), Some(0), Some(0))
    );
    let edited = change(&s.edited_rename, "src/a.rs");
    assert_eq!(
        (
            edited.status.as_str(),
            edited.old_path.as_deref(),
            edited.added,
            edited.deleted
        ),
        ("R", Some("a.rs"), Some(1), Some(1))
    );
    let binary = change(&s.binary, "bin.dat");
    assert_eq!(
        (binary.status.as_str(), binary.added, binary.deleted),
        ("M", None, None)
    );
    assert!(
        changes
            .iter()
            .any(|c| c.path == "kind.txt" && c.status == "T")
    );
    assert!(changes.iter().all(|c| c.sha != s.merge));

    let with_merges = Range {
        include_merges: true,
        ..Range::default()
    };
    let merge_rows: Vec<_> = history::file_changes(&repo, &with_merges)
        .unwrap()
        .into_iter()
        .filter(|c| c.sha == s.merge)
        .collect();
    assert_eq!(merge_rows.len(), 1);
    assert_eq!(
        (merge_rows[0].path.as_str(), merge_rows[0].status.as_str()),
        ("side.rs", "A")
    );

    let commits = history::commits(&repo, &all).unwrap();
    assert_eq!(commits.len(), 11);
    assert_eq!(commits[0].sha, s.merge);
    assert!(commits[0].is_merge && commits[0].parent_count == 2);
    let wrapped = commits.iter().find(|c| c.sha == s.multi_hunk).unwrap();
    assert_eq!(wrapped.subject, "First line wraps here");
    assert_eq!(wrapped.author_name, "Fixture Author");
    assert_eq!(commits.last().unwrap().author_time, 1_700_003_600);

    let since = Range {
        since: Some(s.edited_rename.clone()),
        ..Range::default()
    };
    assert_eq!(history::commits(&repo, &since).unwrap().len(), 6);
}
