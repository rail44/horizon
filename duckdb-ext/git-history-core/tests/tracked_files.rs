//! `tracked_files` against the `git` CLI on a fixture worktree.
//!
//! The fixture is built with the `git` CLI under a temporary directory with
//! fixed names and dates and no user or system configuration. Expectations
//! come from `git ls-files -s -z` (tracked files), `git ls-files -o
//! --exclude-standard -z` (untracked files), `git status --porcelain=v2 -z`
//! (worktree modifications and modes) and `git hash-object` (content ids;
//! a symlink's target path through `--stdin`, since `git hash-object PATH`
//! follows the link).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use git_history_core::{self as history, TrackedFileRow};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn command_in(&self, dir: &Path, args: &[&str]) -> Command {
        let date = "@1700000000 +0900";
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join(".no-xdg"))
            .env("GIT_AUTHOR_NAME", "Fixture Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
        command
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_in(self.path(), args)
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Vec<u8> {
        let output = self.command_in(dir, args).output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_in(self.path(), args)
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let path = self.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn chmod_x(&self, path: &str) {
        let path = self.path().join(path);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

const SPACE: &str = "with space.txt";
const NON_ASCII: &str = "名前/ファイル.txt";

/// A worktree with every case `tracked_files` distinguishes.
fn build() -> Fixture {
    let f = Fixture {
        dir: tempfile::tempdir().expect("temp dir"),
    };
    f.git(&["init", "-q", "-b", "main"]);
    f.write(".gitignore", "*.log\n/build/\n");
    f.write(".gitattributes", "*.crlf text eol=crlf\n");
    f.write("same.txt", "unchanged\n");
    f.write("modified.txt", "before\n");
    f.write("staged.txt", "committed\n");
    f.write("deleted.txt", "gone soon\n");
    f.write("run.sh", "#!/bin/sh\necho hi\n");
    f.chmod_x("run.sh");
    f.write("mode.sh", "#!/bin/sh\n");
    f.write("dir/nested/deep.rs", "fn main() {}\n");
    f.write(SPACE, "space\n");
    f.write(NON_ASCII, "非ASCII\n");
    f.write("eol.crlf", "one\r\ntwo\r\n");
    std::os::unix::fs::symlink("same.txt", f.path().join("link")).unwrap();
    f.git(&["add", "-A"]);
    // A gitlink with no submodule repository behind it.
    let head_less = "1111111111111111111111111111111111111111";
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head_less},vendored"),
    ]);
    fs::create_dir(f.path().join("vendored")).unwrap();
    f.git(&["commit", "-q", "-m", "Fixture"]);

    // Worktree changes.
    f.write("modified.txt", "after\n");
    f.write("staged.txt", "staged\n");
    f.git(&["add", "staged.txt"]);
    f.write("staged.txt", "changed again\n");
    fs::remove_file(f.path().join("deleted.txt")).unwrap();
    f.chmod_x("mode.sh");

    // Untracked, ignored and excluded files.
    f.write("untracked.txt", "new\n");
    f.write("newdir/inner/new.rs", "// new\n");
    f.write("new.crlf", "a\r\nb\r\n");
    f.write("tool.sh", "#!/bin/sh\n");
    f.chmod_x("tool.sh");
    std::os::unix::fs::symlink("untracked.txt", f.path().join("new-link")).unwrap();
    f.write("debug.log", "ignored\n");
    f.write("build/out.o", "ignored dir\n");
    f.write(".git/info/exclude", "*.tmp\n");
    f.write("scratch.tmp", "excluded\n");
    // Kept under `.git` so the excludes file is not itself an untracked file.
    let global_excludes = f.path().join(".git/global-excludes");
    fs::write(&global_excludes, "*.global\n").unwrap();
    f.git(&[
        "config",
        "core.excludesFile",
        global_excludes.to_str().unwrap(),
    ]);
    f.write("editor.global", "excluded by core.excludesFile\n");

    // A nested repository, with a commit, that must not be descended.
    let sub = f.path().join("sub");
    fs::create_dir(&sub).unwrap();
    f.git_in(&sub, &["init", "-q", "-b", "main"]);
    f.write("sub/inner.txt", "inner\n");
    f.git_in(&sub, &["add", "-A"]);
    f.git_in(&sub, &["commit", "-q", "-m", "Inner"]);
    f
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8(s.to_vec()).unwrap())
        .collect()
}

/// `git hash-object PATH`, except that a symlink is hashed as its target
/// path (what `git add` stores) rather than followed.
fn hash_object(f: &Fixture, path: &str) -> String {
    let full = f.path().join(path);
    let output = if fs::symlink_metadata(&full)
        .unwrap()
        .file_type()
        .is_symlink()
    {
        let target = fs::read_link(&full).unwrap();
        let mut child = f
            .command(&["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(target.as_os_str().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap().stdout
    } else {
        f.git(&["hash-object", "--", path])
    };
    String::from_utf8(output).unwrap().trim().to_string()
}

/// The rows `tracked_files` should produce, from the git CLI.
fn expected(f: &Fixture, include_untracked: bool) -> Vec<TrackedFileRow> {
    // Worktree changes by path: (worktree mode, deleted).
    let mut changes = BTreeMap::new();
    for record in split_z(&f.git(&["status", "--porcelain=v2", "-z", "--untracked-files=no"])) {
        let fields: Vec<&str> = record.splitn(9, ' ').collect();
        assert_eq!(fields[0], "1", "unexpected status record {record:?}");
        let worktree_state = fields[1].as_bytes()[1];
        if worktree_state != b'.' {
            changes.insert(
                fields[8].to_string(),
                (fields[5].to_string(), worktree_state == b'D'),
            );
        }
    }
    let mut rows = Vec::new();
    for record in split_z(&f.git(&["ls-files", "-s", "-z"])) {
        let (meta, path) = record.split_once('\t').unwrap();
        let meta: Vec<&str> = meta.split(' ').collect();
        assert_eq!(meta[2], "0", "no unmerged entries in the fixture");
        let (mode, id) = (meta[0].to_string(), meta[1].to_string());
        let row = match changes.get(path) {
            Some((_, true)) => continue,
            Some((worktree_mode, false)) => TrackedFileRow {
                path: path.to_string(),
                mode: worktree_mode.clone(),
                blob_id: hash_object(f, path),
                is_tracked: true,
                is_modified: true,
            },
            None => {
                if mode != "160000" {
                    // Unchanged files hash to their index id.
                    assert_eq!(hash_object(f, path), id, "{path}");
                }
                TrackedFileRow {
                    path: path.to_string(),
                    mode,
                    blob_id: id,
                    is_tracked: true,
                    is_modified: false,
                }
            }
        };
        rows.push(row);
    }
    if include_untracked {
        for path in split_z(&f.git(&["ls-files", "-o", "--exclude-standard", "-z"])) {
            // A nested repository is listed as a directory; it is not a file.
            if path.ends_with('/') {
                continue;
            }
            let meta = fs::symlink_metadata(f.path().join(&path)).unwrap();
            let mode = if meta.file_type().is_symlink() {
                "120000"
            } else if meta.permissions().mode() & 0o100 != 0 {
                "100755"
            } else {
                "100644"
            };
            rows.push(TrackedFileRow {
                blob_id: hash_object(f, &path),
                path,
                mode: mode.to_string(),
                is_tracked: false,
                is_modified: false,
            });
        }
    }
    rows.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    rows
}

fn row<'a>(rows: &'a [TrackedFileRow], path: &str) -> Option<&'a TrackedFileRow> {
    rows.iter().find(|r| r.path == path)
}

#[test]
fn tracked_files_match_git_ls_files() {
    let f = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    let rows = history::tracked_files(&repo, false).unwrap();
    assert_eq!(rows, expected(&f, false));

    let get = |path| row(&rows, path).unwrap_or_else(|| panic!("no row for {path}"));
    assert!(!get("same.txt").is_modified);
    assert!(get("modified.txt").is_modified);
    // Staged then changed again: the worktree content, not the staged blob.
    let staged = get("staged.txt");
    assert!(staged.is_modified);
    assert_eq!(staged.blob_id, hash_object(&f, "staged.txt"));
    assert!(row(&rows, "deleted.txt").is_none());
    assert_eq!(get("run.sh").mode, "100755");
    let mode_only = get("mode.sh");
    assert_eq!(
        (mode_only.mode.as_str(), mode_only.is_modified),
        ("100755", true)
    );
    let link = get("link");
    assert_eq!(link.mode, "120000");
    assert_eq!(link.blob_id, hash_object(&f, "link"));
    assert_eq!(get("vendored").mode, "160000");
    assert_eq!(
        get("vendored").blob_id,
        "1111111111111111111111111111111111111111"
    );
    // CRLF in the worktree, normalized by the `text` attribute: unchanged.
    assert!(!get("eol.crlf").is_modified);
    assert!(row(&rows, SPACE).is_some() && row(&rows, NON_ASCII).is_some());
    assert!(row(&rows, "dir/nested/deep.rs").is_some());
    assert!(rows.iter().all(|r| r.is_tracked));
}

#[test]
fn untracked_files_match_git_ls_files_others() {
    let f = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    let rows = history::tracked_files(&repo, true).unwrap();
    assert_eq!(rows, expected(&f, true));

    let untracked: Vec<&str> = rows
        .iter()
        .filter(|r| !r.is_tracked)
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(
        untracked,
        [
            "new-link",
            "new.crlf",
            "newdir/inner/new.rs",
            "tool.sh",
            "untracked.txt"
        ]
    );
    // The tracked rows are the same with and without untracked files.
    assert_eq!(
        rows.iter()
            .filter(|r| r.is_tracked)
            .cloned()
            .collect::<Vec<_>>(),
        history::tracked_files(&repo, false).unwrap()
    );
    // git lists the nested repository as `sub/`; it has no file rows.
    let others = split_z(&f.git(&["ls-files", "-o", "--exclude-standard", "-z"]));
    assert!(others.contains(&"sub/".to_string()));
    assert!(rows.iter().all(|r| !r.path.starts_with("sub/")));
    assert!(rows.iter().all(|r| !r.path.starts_with(".git/")));
    // The CRLF content is hashed after the clean filter, as `git hash-object` does.
    let crlf = row(&rows, "new.crlf").unwrap();
    assert_eq!(crlf.blob_id, hash_object(&f, "new.crlf"));
    let lf = String::from_utf8(f.git(&["hash-object", "--no-filters", "new.crlf"])).unwrap();
    assert_ne!(crlf.blob_id, lf.trim());
}

#[test]
fn an_unmerged_path_has_one_row_for_its_worktree_content() {
    let f = Fixture {
        dir: tempfile::tempdir().expect("temp dir"),
    };
    f.git(&["init", "-q", "-b", "main"]);
    f.write("conflict.txt", "base\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "Base"]);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("conflict.txt", "side\n");
    f.git(&["commit", "-q", "-am", "Side"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("conflict.txt", "main\n");
    f.git(&["commit", "-q", "-am", "Main"]);
    let merge = f.command(&["merge", "-q", "side"]).output().unwrap();
    assert!(!merge.status.success(), "the merge must conflict");
    assert_eq!(split_z(&f.git(&["ls-files", "-s", "-z"])).len(), 3);

    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    assert_eq!(
        history::tracked_files(&repo, false).unwrap(),
        [TrackedFileRow {
            path: "conflict.txt".into(),
            mode: "100644".into(),
            blob_id: hash_object(&f, "conflict.txt"),
            is_tracked: true,
            is_modified: true,
        }]
    );
}

#[test]
fn repeated_runs_are_identical() {
    let f = build();
    let repo = history::open(f.path().to_str().unwrap()).unwrap();
    assert_eq!(
        history::tracked_files(&repo, true).unwrap(),
        history::tracked_files(&repo, true).unwrap()
    );
}
