//! The probe's rows against the `git` CLI on the fixture repository of
//! `git-history/tests/cli_parity.rs`, rebuilt here so that crate stays
//! untouched. The fixture builder and the CLI parsers are copies of that
//! test's; the assertions record, per fixture commit, where gix's rows equal
//! git's and where they do not.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use gix_probe::{CommitRow, FileChangeRow, HunkRow, Options};

/// The two settings of `git-history`'s `Range`, which the copied parsers take.
#[derive(Debug, Clone, Default)]
struct Range {
    since: Option<String>,
    include_merges: bool,
}

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

// Copied with the builder; not every case is looked up by its sha here.
#[allow(dead_code)]
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

fn options(range: &Range) -> Options {
    Options {
        since: range.since.clone(),
        include_merges: range.include_merges,
        ..Options::default()
    }
}

/// The subjects of the fixture commits whose rows differ between git and gix.
fn differing<T: Ord + Clone>(
    git: &[T],
    gix: &[T],
    sha: impl Fn(&T) -> &str,
    subjects: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut by_commit: BTreeMap<&str, (Vec<T>, Vec<T>)> = BTreeMap::new();
    for row in git {
        by_commit.entry(sha(row)).or_default().0.push(row.clone());
    }
    for row in gix {
        by_commit.entry(sha(row)).or_default().1.push(row.clone());
    }
    let mut out: Vec<String> = by_commit
        .into_iter()
        .filter_map(|(sha, (mut a, mut b))| {
            a.sort();
            b.sort();
            (a != b).then(|| subjects[sha].clone())
        })
        .collect();
    out.sort();
    out
}

#[test]
fn fixture_cases_against_git_cli() {
    let (f, shas) = build();
    let repo = gix_probe::open(f.path().join("src").to_str().unwrap()).unwrap();
    let subjects: BTreeMap<String, String> = cli_commits(&f, &Range::default())
        .into_iter()
        .map(|c| (c.sha, c.subject))
        .collect();
    for range in ranges(&shas) {
        let opts = options(&range);
        let commits: Vec<CommitRow> = gix_probe::commits(&repo, &opts).unwrap();
        assert_eq!(commits, cli_commits(&f, &range), "commits {range:?}");

        // Every case but the type change: root commit, multi-hunk, insertion-
        // and deletion-only, renames with and without edits, the binary file
        // (no hunks), the merge with and without first-parent diffs.
        let git = cli_hunks(&f, &range);
        let gix = gix_probe::hunks(&repo, &opts).unwrap();
        let hunk_cases = differing(&git, &gix, |h: &HunkRow| &h.sha, &subjects);
        assert_eq!(hunk_cases, ["Make kind.txt a symlink"], "hunks {range:?}");
        let kind = |rows: &[HunkRow]| -> Vec<[i32; 4]> {
            rows.iter()
                .filter(|h| h.path == "kind.txt" && subjects[&h.sha] == "Make kind.txt a symlink")
                .map(|h| [h.old_start, h.old_lines, h.new_start, h.new_lines])
                .collect()
        };
        // git prints the file-to-symlink change as a deletion and a creation;
        // gix's blob platform diffs the two contents as one modification.
        assert_eq!(kind(&git), [[0, 0, 1, 1], [1, 1, 0, 0]]);
        assert_eq!(kind(&gix), [[1, 1, 1, 1]]);

        // Every case, including status T, the pure rename's 0/0 counts and
        // the binary file's NULL counts.
        let git = cli_file_changes(&f, &range);
        let gix = gix_probe::file_changes(&repo, &opts).unwrap();
        let change_cases = differing(&git, &gix, |c: &FileChangeRow| &c.sha, &subjects);
        assert!(
            change_cases.is_empty(),
            "file changes {range:?}: {change_cases:?}"
        );
    }
    let changes = gix_probe::file_changes(&repo, &Options::default()).unwrap();
    let binary = changes
        .iter()
        .find(|c| c.sha == shas.binary && c.path == "bin.dat")
        .unwrap();
    assert_eq!((binary.added, binary.deleted), (None, None));
    let edited = changes
        .iter()
        .find(|c| c.sha == shas.edited_rename)
        .unwrap();
    assert_eq!(
        (
            edited.status.as_str(),
            edited.old_path.as_deref(),
            edited.added
        ),
        ("R", Some("a.rs"), Some(1))
    );
}
