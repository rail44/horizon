use super::*;

fn git_ok(dir: &Path, args: &[&str]) -> String {
    run_git_in(dir, args, &IDENTITY).unwrap()
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git_ok(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("kept.txt"), "kept\n").unwrap();
    std::fs::write(dir.path().join("changed.txt"), "before\n").unwrap();
    std::fs::write(dir.path().join("deleted.txt"), "deleted\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    git_ok(dir.path(), &["add", "-A"]);
    git_ok(dir.path(), &["commit", "-q", "-m", "root"]);
    dir
}

/// Changes a tracked file, deletes one, adds an untracked and an ignored one.
fn change(dir: &Path) {
    std::fs::write(dir.join("changed.txt"), "after\n").unwrap();
    std::fs::remove_file(dir.join("deleted.txt")).unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    std::fs::write(dir.join("ignored.txt"), "ignored\n").unwrap();
}

fn read(dir: &Path, file: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(file)).ok()
}

#[test]
fn a_checkpoint_holds_the_work_tree_and_leaves_the_repository_as_it_was() {
    let repo = repo();
    let dir = repo.path();
    change(dir);
    let head = git_ok(dir, &["rev-parse", "HEAD"]);
    let status = git_ok(dir, &["status", "--porcelain"]);
    let session = Uuid::new_v4();

    let commit = record(dir, session, "record-1").unwrap().unwrap();

    assert_eq!(git_ok(dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_ok(dir, &["status", "--porcelain"]), status);
    assert_eq!(find(dir, session, "record-1"), Some(commit.clone()));
    assert_eq!(git_ok(dir, &["rev-parse", &format!("{commit}^")]), head);
    let files = git_ok(dir, &["ls-tree", "--name-only", &commit]);
    let files: Vec<_> = files.lines().collect();
    assert_eq!(files, [".gitignore", "changed.txt", "kept.txt", "new.txt"]);
    assert_eq!(
        git_ok(dir, &["show", &format!("{commit}:changed.txt")]),
        "after"
    );
}

#[test]
fn restoring_a_checkpoint_brings_its_files_back_and_keeps_head() {
    let repo = repo();
    let dir = repo.path();
    change(dir);
    let session = Uuid::new_v4();
    let commit = record(dir, session, "record-1").unwrap().unwrap();
    std::fs::write(dir.join("changed.txt"), "later\n").unwrap();
    std::fs::remove_file(dir.join("new.txt")).unwrap();
    std::fs::write(dir.join("deleted.txt"), "back\n").unwrap();
    git_ok(dir, &["add", "deleted.txt"]);
    git_ok(dir, &["commit", "-q", "-m", "later"]);
    let head = git_ok(dir, &["rev-parse", "HEAD"]);

    restore(dir, &commit).unwrap();

    assert_eq!(read(dir, "changed.txt").as_deref(), Some("after\n"));
    assert_eq!(read(dir, "new.txt").as_deref(), Some("new\n"));
    assert_eq!(read(dir, "deleted.txt"), None);
    assert_eq!(read(dir, "ignored.txt").as_deref(), Some("ignored\n"));
    assert_eq!(git_ok(dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_ok(dir, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn removing_a_sessions_checkpoints_leaves_the_others() {
    let repo = repo();
    let dir = repo.path();
    let (mine, other) = (Uuid::new_v4(), Uuid::new_v4());
    record(dir, mine, "a").unwrap();
    record(dir, mine, "b").unwrap();
    record(dir, other, "c").unwrap();

    remove_all(dir, mine).unwrap();

    assert_eq!(find(dir, mine, "a"), None);
    assert_eq!(find(dir, mine, "b"), None);
    assert!(find(dir, other, "c").is_some());
}

#[test]
fn a_directory_outside_a_git_work_tree_has_no_checkpoint_outside_a_git_repository() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(record(dir.path(), Uuid::new_v4(), "a"), Ok(None));
}
