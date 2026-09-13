use super::*;

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    run(root.path(), &["init", "-q", "-b", "main"]).unwrap();
    run(
        root.path(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    )
    .unwrap();
    root
}
fn branch(root: &Path, name: &str) -> Worker {
    let path = root.join(".worktrees").join(name);
    // Fixture-local excludes do not affect any enclosing checkout.
    std::fs::write(root.join(".git/info/exclude"), "/.worktrees/\n").unwrap();
    run(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            path.to_str().unwrap(),
            "main",
        ],
    )
    .unwrap();
    Worker {
        session: name.into(),
        worktree: path.to_string_lossy().into_owned(),
        branch: name.into(),
    }
}
fn commit(worker: &Worker, name: &str) {
    let path = Path::new(&worker.worktree);
    std::fs::write(path.join(name), name).unwrap();
    run(path, &["add", name]).unwrap();
    run(
        path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "-qm",
            name,
        ],
    )
    .unwrap();
}

#[test]
fn main_advancing_requires_new_combined_verification_and_merge_is_recoverable() {
    let root = fixture();
    let a = branch(root.path(), "a");
    let b = branch(root.path(), "b");
    commit(&a, "a.txt");
    commit(&b, "b.txt");
    let ca = prepare(root.path(), &a).unwrap();
    let cb = prepare(root.path(), &b).unwrap();
    assert!(matches!(
        integrate(root.path(), &a, &ca).unwrap(),
        MergeOutcome::Integrated
    ));
    assert!(matches!(
        integrate(root.path(), &b, &cb).unwrap(),
        MergeOutcome::Stale
    ));
    // Merge commits use the fixture identity without repository/global config.
    run(
        Path::new(&b.worktree),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "merge",
            "--no-edit",
            &ca.head,
        ],
    )
    .unwrap();
    let cb = prepare(root.path(), &b).unwrap();
    assert_eq!(cb.base, ca.head);
    assert!(matches!(
        integrate(root.path(), &b, &cb).unwrap(),
        MergeOutcome::Integrated
    ));
    assert!(matches!(
        integrate(root.path(), &b, &cb).unwrap(),
        MergeOutcome::Integrated
    ));
    assert!(root.path().join("a.txt").is_file() && root.path().join("b.txt").is_file());
}

#[test]
fn dirty_main_and_changed_candidates_are_preserved() {
    let root = fixture();
    let a = branch(root.path(), "a");
    commit(&a, "a.txt");
    let candidate = prepare(root.path(), &a).unwrap();
    let original = run(root.path(), &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(root.path().join("owner.txt"), "owner work").unwrap();
    assert!(integrate(root.path(), &a, &candidate).is_err());
    assert_eq!(run(root.path(), &["rev-parse", "HEAD"]).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(root.path().join("owner.txt")).unwrap(),
        "owner work"
    );
    std::fs::remove_file(root.path().join("owner.txt")).unwrap();
    commit(&a, "later.txt");
    assert!(integrate(root.path(), &a, &candidate).is_err());
    assert_eq!(run(root.path(), &["rev-parse", "HEAD"]).unwrap(), original);
}

#[test]
fn actual_changed_paths_must_fit_the_declared_scope() {
    let root = fixture();
    let worker = branch(root.path(), "scope");
    commit(&worker, "unexpected.txt");
    let scope = horizon_board::workflow::ChangeScope {
        paths: vec!["expected.txt".into()],
        functions: vec!["fixture".into()],
    };
    assert!(check_scope(root.path(), &worker, &scope)
        .unwrap_err()
        .contains("unexpected.txt"));
}
