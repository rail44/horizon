#![cfg(target_os = "linux")]

use std::process::Command;

use horizon_sandbox::{FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope};

#[test]
fn helper_preserves_exit_status_and_publishes_an_authenticated_report() {
    let root = test_dir("exit-report");
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("exit 23");
    let sandboxed =
        horizon_sandbox::spawn(command, &policy, horizon_sandbox::SandboxStdio::inherit())
            .expect("spawn supervised sandbox");
    let report = sandboxed
        .supervisor_report
        .expect("Linux spawn returns a supervisor report");
    let mut child = sandboxed.child;

    let status = child.wait().expect("wait for helper");
    assert_eq!(status.code(), Some(23));
    let outcome = report.read().expect("read authenticated report");
    assert_eq!(outcome.exit_code, 23);
    assert!(outcome.approvals.is_empty());

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn missing_create_denial_is_structured_even_when_shell_exits_zero() {
    let writable = test_dir("landlock-writable");
    let outside = test_dir("landlock-outside");
    let target = outside.join("must-not-exist");
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("printf no > {} || true", shell_quote(&target)));
    let sandboxed =
        horizon_sandbox::spawn(command, &policy, horizon_sandbox::SandboxStdio::inherit())
            .expect("spawn supervised sandbox");
    let report = sandboxed
        .supervisor_report
        .expect("Linux spawn returns a supervisor report");
    let mut child = sandboxed.child;
    let status = child.wait().expect("wait for helper");

    assert_eq!(status.code(), Some(0));
    assert!(!target.exists());
    let outcome = report.read().expect("read report");
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.approvals.len(), 1);
    let expected_path = outside
        .canonicalize()
        .expect("outside directory exists")
        .join("must-not-exist");
    match &outcome.approvals[0].request {
        nono::ApprovalRequest::Capability { path, access, .. } => {
            assert_eq!(path, &expected_path);
            assert_eq!(*access, nono::AccessMode::Write);
        }
        other => panic!("expected a filesystem capability request, got {other:?}"),
    }

    std::fs::remove_dir_all(writable).expect("remove writable directory");
    std::fs::remove_dir_all(outside).expect("remove outside directory");
}

#[test]
fn exact_file_grant_allows_that_file_but_not_its_sibling() {
    let writable = test_dir("exact-file-writable");
    let outside = test_dir("exact-file-outside");
    let target = outside.join("approved.txt");
    let sibling = outside.join("not-approved.txt");
    std::fs::write(&target, "before").expect("create approved target");
    let grant = FilesystemGrant {
        path: target.canonicalize().expect("canonical target"),
        access: FilesystemGrantAccess::ReadWrite,
        scope: FilesystemGrantScope::File,
    };
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "printf approved > {}; printf denied > {} || true",
        shell_quote(&target),
        shell_quote(&sibling)
    ));
    let sandboxed = horizon_sandbox::spawn_with_filesystem_grants(
        command,
        &policy,
        &[grant],
        horizon_sandbox::SandboxStdio::inherit(),
    )
    .expect("spawn with exact file grant");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;

    assert_eq!(child.wait().expect("wait").code(), Some(0));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved");
    assert!(!sibling.exists());
    let outcome = report.read().expect("read report");
    assert!(outcome.approvals.iter().any(|approval| matches!(
        &approval.request,
        nono::ApprovalRequest::Capability { path, .. } if path == &sibling
    )));

    std::fs::remove_dir_all(writable).expect("remove writable directory");
    std::fs::remove_dir_all(outside).expect("remove outside directory");
}

#[test]
fn directory_tree_grant_allows_creating_a_missing_suffix() {
    let writable = test_dir("tree-writable");
    let outside = test_dir("tree-outside");
    let target = outside.join("new").join("leaf.txt");
    let grant = FilesystemGrant {
        path: outside.canonicalize().expect("canonical outside"),
        access: FilesystemGrantAccess::ReadWrite,
        scope: FilesystemGrantScope::DirectoryTree,
    };
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "mkdir -p {} && printf created > {}",
        shell_quote(target.parent().unwrap()),
        shell_quote(&target)
    ));
    let sandboxed = horizon_sandbox::spawn_with_filesystem_grants(
        command,
        &policy,
        &[grant],
        horizon_sandbox::SandboxStdio::inherit(),
    )
    .expect("spawn with directory tree grant");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;

    assert_eq!(child.wait().expect("wait").code(), Some(0));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "created");
    assert!(report.read().expect("read report").approvals.is_empty());

    std::fs::remove_dir_all(writable).expect("remove writable directory");
    std::fs::remove_dir_all(outside).expect("remove outside directory");
}

fn test_dir(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "horizon-supervised-helper-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir(&path).expect("create test directory");
    path
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
