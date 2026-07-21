#![cfg(target_os = "linux")]

use std::io::Read;
use std::net::{TcpListener, UdpSocket};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::time::Duration;

use horizon_sandbox::{FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope};

const NETWORK_PROBE: &str = env!("CARGO_BIN_EXE_horizon-sandbox-network-probe");

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

#[test]
fn standard_dev_null_is_read_write_but_other_devices_are_not() {
    let root = test_dir("dev-null-baseline");
    let marker = root.join("dev-null-worked");
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "printf discarded > /dev/null && printf ok > {}; \
         if printf forbidden > /dev/zero 2>/dev/null; then exit 24; fi",
        shell_quote(&marker)
    ));
    let sandboxed =
        horizon_sandbox::spawn(command, &policy, horizon_sandbox::SandboxStdio::inherit())
            .expect("spawn device baseline probe");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;

    assert_eq!(child.wait().expect("wait").code(), Some(0));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ok");
    let outcome = report.read().expect("read report");
    assert!(outcome.approvals.iter().any(|approval| matches!(
        &approval.request,
        nono::ApprovalRequest::Capability { path, .. } if path == std::path::Path::new("/dev/zero")
    )));
    assert!(!outcome.approvals.iter().any(|approval| matches!(
        &approval.request,
        nono::ApprovalRequest::Capability { path, .. } if path == std::path::Path::new("/dev/null")
    )));

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn git_status_can_open_its_standard_dev_null_endpoint() {
    let root = test_dir("git-dev-null");
    let mut init = Command::new("git");
    init.arg("init").arg("--quiet").arg(&root);
    scrub_git_env(&mut init);
    let init = init.status().expect("run host git init");
    assert!(init.success(), "initialize test repository");

    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("git");
    command.arg("status").arg("--short").current_dir(&root);
    scrub_git_env(&mut command);
    let sandboxed = horizon_sandbox::spawn(
        command,
        &policy,
        horizon_sandbox::SandboxStdio::piped_output(),
    )
    .expect("spawn sandboxed git status");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let output = sandboxed.child.wait_with_output().expect("wait for git");

    assert!(
        output.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.read().expect("read report").approvals.is_empty());

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn proxy_policy_allows_only_the_exact_ipv4_tcp_endpoint() {
    let root = test_dir("proxy-exact");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let decoy = TcpListener::bind(("127.0.0.2", proxy_addr.port())).expect("bind decoy");

    let allowed = run_network_probe(&root, proxy_addr, "tcp", &proxy_addr.to_string());
    assert_eq!(
        allowed.0,
        Some(0),
        "exact endpoint should connect: {allowed:?}"
    );

    let decoy_addr = decoy.local_addr().unwrap();
    let denied = run_network_probe(&root, proxy_addr, "tcp", &decoy_addr.to_string());
    assert_eq!(
        denied.0,
        Some(23),
        "same-port decoy must be denied: {denied:?}"
    );
    assert!(denied.2.ipc_denials.iter().any(|record| {
        record.target == decoy_addr.to_string() && record.operation == "connect"
    }));

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn proxy_policy_denies_udp_even_at_the_tcp_proxy_port() {
    let root = test_dir("proxy-udp");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let udp = UdpSocket::bind(proxy_addr).expect("bind same-port UDP decoy");
    udp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let denied = run_network_probe(&root, proxy_addr, "udp", &proxy_addr.to_string());
    assert_eq!(denied.0, Some(23), "UDP socket must be denied: {denied:?}");
    let mut packet = [0_u8; 64];
    assert!(
        udp.recv(&mut packet).is_err(),
        "UDP payload escaped containment"
    );

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn proxy_policy_denies_pathname_unix_sockets() {
    let root = test_dir("proxy-unix");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let socket_path = root.join("outside.sock");
    let _listener = UnixListener::bind(&socket_path).expect("bind unix decoy");

    let denied = run_network_probe(
        &root,
        proxy_addr,
        "unix",
        &socket_path.display().to_string(),
    );
    assert_eq!(
        denied.0,
        Some(23),
        "pathname UDS must be denied: {denied:?}"
    );
    assert!(denied.2.ipc_denials.iter().any(|record| {
        record.target == socket_path.display().to_string() && record.operation == "connect"
    }));

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn one_report_contains_filesystem_and_network_denials() {
    let writable = test_dir("combined-writable");
    let outside = test_dir("combined-outside");
    let target = outside.join("denied.txt");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let decoy = TcpListener::bind(("127.0.0.2", proxy_addr.port())).expect("bind decoy");
    let decoy_addr = decoy.local_addr().unwrap();
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Proxied { proxy_addr },
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "printf denied > {} || true; {} tcp {} || true",
        shell_quote(&target),
        NETWORK_PROBE,
        decoy_addr
    ));
    let sandboxed = horizon_sandbox::spawn(
        command,
        &policy,
        horizon_sandbox::SandboxStdio::piped_output(),
    )
    .expect("spawn combined probe");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert!(!target.exists());
    let outcome = report.read().expect("read combined report");
    assert!(!outcome.approvals.is_empty(), "filesystem denial missing");
    assert!(!outcome.ipc_denials.is_empty(), "network denial missing");

    std::fs::remove_dir_all(writable).expect("remove writable directory");
    std::fs::remove_dir_all(outside).expect("remove outside directory");
}

fn run_network_probe(
    root: &std::path::Path,
    proxy_addr: std::net::SocketAddr,
    mode: &str,
    target: &str,
) -> (
    Option<i32>,
    String,
    horizon_sandbox_runtime::SupervisedOutcome,
) {
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.to_path_buf()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Proxied { proxy_addr },
    };
    let mut command = Command::new(NETWORK_PROBE);
    command.arg(mode).arg(target);
    let sandboxed = horizon_sandbox::spawn(
        command,
        &policy,
        horizon_sandbox::SandboxStdio::piped_output(),
    )
    .expect("spawn network probe");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let output = sandboxed
        .child
        .wait_with_output()
        .expect("wait network probe");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let outcome = report.read().expect("read network report");
    (output.status.code(), stderr, outcome)
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

fn scrub_git_env(command: &mut Command) {
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}
