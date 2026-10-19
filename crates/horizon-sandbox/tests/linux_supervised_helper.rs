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
        writable_root_exclusions: Vec::new(),
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
        writable_root_exclusions: Vec::new(),
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
        excluded_subpaths: Vec::new(),
    };
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        writable_root_exclusions: Vec::new(),
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
        excluded_subpaths: Vec::new(),
    };
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![writable.clone()],
        writable_root_exclusions: Vec::new(),
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
        writable_root_exclusions: Vec::new(),
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
        writable_root_exclusions: Vec::new(),
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
fn loopback_connect_grant_allows_the_granted_endpoint() {
    let root = test_dir("loopback-grant");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let sccache = TcpListener::bind("127.0.0.1:0").expect("bind granted endpoint");
    let sccache_addr = sccache.local_addr().unwrap();

    let allowed = run_network_probe_with_loopback(
        &root,
        proxy_addr,
        &[sccache_addr],
        "tcp",
        &sccache_addr.to_string(),
    );
    assert_eq!(
        allowed.0,
        Some(0),
        "granted loopback endpoint should connect: {allowed:?}"
    );

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn loopback_connect_grant_denies_a_non_granted_port() {
    let root = test_dir("loopback-deny");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let sccache = TcpListener::bind("127.0.0.1:0").expect("bind granted endpoint");
    let sccache_addr = sccache.local_addr().unwrap();
    // A different loopback port that is NOT in the grant.
    let other = TcpListener::bind("127.0.0.1:0").expect("bind non-granted endpoint");
    let other_addr = other.local_addr().unwrap();

    let denied = run_network_probe_with_loopback(
        &root,
        proxy_addr,
        &[sccache_addr],
        "tcp",
        &other_addr.to_string(),
    );
    assert_eq!(
        denied.0,
        Some(23),
        "non-granted loopback port must be denied: {denied:?}"
    );
    assert!(denied.2.ipc_denials.iter().any(|record| {
        record.target == other_addr.to_string() && record.operation == "connect"
    }));

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn loopback_connect_grant_does_not_allow_bind() {
    let root = test_dir("loopback-bind");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let sccache = TcpListener::bind("127.0.0.1:0").expect("bind granted endpoint");
    let sccache_addr = sccache.local_addr().unwrap();
    drop(sccache);

    let denied = run_network_probe_with_loopback(
        &root,
        proxy_addr,
        &[sccache_addr],
        "bind",
        &sccache_addr.to_string(),
    );
    assert_eq!(
        denied.0,
        Some(23),
        "bind must stay denied even with a loopback_connect grant: {denied:?}"
    );
    assert!(denied
        .2
        .ipc_denials
        .iter()
        .any(|record| { record.target == sccache_addr.to_string() && record.operation == "bind" }));

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
        writable_root_exclusions: Vec::new(),
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Proxied {
            proxy_addr,
            loopback_connect: Vec::new(),
            unix_socket_connect: Vec::new(),
        },
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

#[test]
fn proxy_connects_from_a_non_main_thread_reach_only_the_trusted_endpoint() {
    let root = test_dir("proxy-thread");
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy endpoint");
    let proxy_addr = proxy.local_addr().unwrap();
    let decoy = TcpListener::bind(("127.0.0.2", proxy_addr.port())).expect("bind decoy");
    let decoy_addr = decoy.local_addr().unwrap();

    // Regression: the seccomp-notify pid is the connecting thread's TID,
    // and pidfd_open rejects non-leader TIDs (ENOENT), so every proxy
    // connect from a non-main thread of a multithreaded child was denied
    // with "could not inspect the child socket". It must reach the exact
    // proxy endpoint -- and the exact-match policy must stay intact
    // otherwise.
    let allowed = run_network_probe(&root, proxy_addr, "thread", &proxy_addr.to_string());
    assert_eq!(
        allowed.0,
        Some(0),
        "a connect from a non-main thread must reach the exact proxy endpoint: {allowed:?}"
    );
    assert!(
        allowed.2.ipc_denials.is_empty(),
        "no denials expected for the proxy connect: {:?}",
        allowed.2.ipc_denials
    );

    let denied = run_network_probe(&root, proxy_addr, "thread", &decoy_addr.to_string());
    assert_eq!(
        denied.0,
        Some(23),
        "a same-port decoy must stay denied from a non-main thread: {denied:?}"
    );
    assert!(denied
        .2
        .ipc_denials
        .iter()
        .any(|record| record.target == decoy_addr.to_string() && record.operation == "connect"));

    std::fs::remove_dir_all(root).expect("remove test directory");
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
    run_network_probe_with_loopback(root, proxy_addr, &[], mode, target)
}

fn run_network_probe_with_loopback(
    root: &std::path::Path,
    proxy_addr: std::net::SocketAddr,
    loopback_connect: &[std::net::SocketAddr],
    mode: &str,
    target: &str,
) -> (
    Option<i32>,
    String,
    horizon_sandbox_runtime::SupervisedOutcome,
) {
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.to_path_buf()],
        writable_root_exclusions: Vec::new(),
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Proxied {
            proxy_addr,
            loopback_connect: loopback_connect.to_vec(),
            unix_socket_connect: Vec::new(),
        },
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

#[test]
fn writable_root_exclusion_denies_writes_beneath_it_and_keeps_the_rest_writable() {
    let root = test_dir("root-exclusion").canonicalize().unwrap();
    let git_dir = root.join(".git");
    std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
    let hook = git_dir.join("hooks").join("pre-commit");
    let config = git_dir.join("config");
    let source = root.join("source.txt");
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.clone()],
        writable_root_exclusions: vec![git_dir.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "printf ok > {}; printf hook > {} || true; printf cfg > {} || true",
        shell_quote(&source),
        shell_quote(&hook),
        shell_quote(&config)
    ));
    let sandboxed =
        horizon_sandbox::spawn(command, &policy, horizon_sandbox::SandboxStdio::inherit())
            .expect("spawn supervised sandbox");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;

    assert_eq!(child.wait().expect("wait").code(), Some(0));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "ok");
    assert!(
        !hook.exists(),
        "a write under the excluded gitdir must be denied"
    );
    assert!(
        !config.exists(),
        "a write under the excluded gitdir must be denied"
    );
    let outcome = report.read().expect("read report");
    for denied in [&hook, &config] {
        assert!(
            outcome.approvals.iter().any(|approval| matches!(
                &approval.request,
                nono::ApprovalRequest::Capability { path, .. } if path == denied
            )),
            "{} must be reported as a denial",
            denied.display()
        );
    }

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn a_tree_grant_on_the_excluded_path_lifts_it_but_keeps_its_own_exclusions() {
    let root = test_dir("root-exclusion-grant").canonicalize().unwrap();
    let git_dir = root.join(".git");
    std::fs::create_dir_all(git_dir.join("hooks")).unwrap();
    std::fs::create_dir_all(git_dir.join("objects")).unwrap();
    let object = git_dir.join("objects").join("new-object");
    let hook = git_dir.join("hooks").join("pre-commit");
    let config = git_dir.join("config");
    let grant = FilesystemGrant {
        path: git_dir.clone(),
        access: FilesystemGrantAccess::ReadWrite,
        scope: FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: vec!["hooks".into(), "config".into()],
    };
    let policy = horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.clone()],
        writable_root_exclusions: vec![git_dir.clone()],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!(
        "printf obj > {}; printf hook > {} || true; printf cfg > {} || true",
        shell_quote(&object),
        shell_quote(&hook),
        shell_quote(&config)
    ));
    let sandboxed = horizon_sandbox::spawn_with_filesystem_grants(
        command,
        &policy,
        &[grant],
        horizon_sandbox::SandboxStdio::inherit(),
    )
    .expect("spawn with metadata grant");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let mut child = sandboxed.child;

    assert_eq!(child.wait().expect("wait").code(), Some(0));
    assert_eq!(std::fs::read_to_string(&object).unwrap(), "obj");
    assert!(!hook.exists());
    assert!(!config.exists());
    report.read().expect("read report");

    std::fs::remove_dir_all(root).expect("remove test directory");
}

/// A root whose `.git` holds `hooks/` and a `config`, plus two staged files.
fn excluded_gitdir_fixture(label: &str) -> std::path::PathBuf {
    let root = test_dir(label).canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
    std::fs::write(root.join(".git/config"), "cfg").unwrap();
    std::fs::write(root.join("staged"), "payload").unwrap();
    std::fs::write(root.join("plain.txt"), "plain").unwrap();
    root
}

fn root_with_excluded_gitdir(root: &std::path::Path) -> horizon_sandbox::SandboxPolicy {
    horizon_sandbox::SandboxPolicy {
        writable_roots: vec![root.to_path_buf()],
        writable_root_exclusions: vec![root.join(".git")],
        readable_scope: horizon_sandbox::ReadableScope::Full,
        network: horizon_sandbox::NetworkPolicy::Disabled,
    }
}

/// Runs `script` under `sh -c` in `root` and returns its exit code, stderr,
/// and the supervisor's report.
fn run_script_in(
    root: &std::path::Path,
    policy: &horizon_sandbox::SandboxPolicy,
    grants: &[FilesystemGrant],
    script: &str,
) -> (
    Option<i32>,
    String,
    horizon_sandbox_runtime::SupervisedOutcome,
) {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script).current_dir(root);
    scrub_git_env(&mut command);
    let sandboxed = horizon_sandbox::spawn_with_filesystem_grants(
        command,
        policy,
        grants,
        horizon_sandbox::SandboxStdio::piped_output(),
    )
    .expect("spawn supervised sandbox");
    let report = sandboxed.supervisor_report.expect("supervisor report");
    let output = sandboxed.child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (
        output.status.code(),
        stderr,
        report.read().expect("read report"),
    )
}

fn reported_paths(outcome: &horizon_sandbox_runtime::SupervisedOutcome) -> Vec<std::path::PathBuf> {
    outcome
        .approvals
        .iter()
        .filter_map(|approval| match &approval.request {
            nono::ApprovalRequest::Capability { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn path_mutations_into_or_out_of_an_excluded_gitdir_are_denied_and_reported() {
    let root = excluded_gitdir_fixture("mutation-denied");
    let git_dir = root.join(".git");
    let hook = git_dir.join("hooks/pre-commit");
    let config = git_dir.join("config");
    let policy = root_with_excluded_gitdir(&root);
    let probe_truncate = format!(
        "{} truncate .git/config",
        shell_quote(std::path::Path::new(NETWORK_PROBE))
    );
    let cases = [
        ("mv staged .git/hooks/pre-commit", hook.clone()),
        ("ln staged .git/hooks/pre-commit", hook.clone()),
        ("ln -s /tmp/x .git/hooks/pre-commit", hook.clone()),
        ("mkdir .git/foo", git_dir.join("foo")),
        ("rm -f .git/config", config.clone()),
        ("mv .git/config moved-config", config.clone()),
        ("truncate -s 0 .git/config", config.clone()),
        (probe_truncate.as_str(), config.clone()),
        // Replacing the excluded directory itself.
        ("mv .git moved-git", git_dir.clone()),
        // Indirections the supervisor must resolve as the sandboxed process
        // does: its own `/proc/self` (through `/dev/fd`) and a symlinked
        // parent directory.
        (
            "exec 3<.git/hooks; mv staged /dev/fd/3/pre-commit",
            hook.clone(),
        ),
        (
            "ln -sfn .git/hooks hooks-link; mv staged hooks-link/pre-commit",
            hook.clone(),
        ),
    ];

    for (script, denied) in cases {
        let (code, stderr, outcome) = run_script_in(&root, &policy, &[], script);
        assert_ne!(code, Some(0), "`{script}` must fail; stderr: {stderr}");
        assert!(
            reported_paths(&outcome).contains(&denied),
            "`{script}` must report {}; got {:?}",
            denied.display(),
            reported_paths(&outcome)
        );
        assert!(
            std::fs::symlink_metadata(&hook).is_err(),
            "`{script}` planted a hook"
        );
        assert!(!git_dir.join("foo").exists(), "`{script}` created .git/foo");
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "cfg",
            "`{script}` changed .git/config"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("staged")).unwrap(),
            "payload"
        );
        assert!(!root.join("moved-config").exists());
        assert!(!root.join("moved-git").exists());
    }

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn the_same_path_mutations_outside_the_excluded_gitdir_still_succeed() {
    let root = excluded_gitdir_fixture("mutation-allowed");
    let policy = root_with_excluded_gitdir(&root);
    let probe = shell_quote(std::path::Path::new(NETWORK_PROBE));
    let script = format!(
        "set -e; mkdir dir; mv staged dir/moved; ln dir/moved hardlink; \
         ln -s /tmp/x symlink; truncate -s 0 plain.txt; printf again > plain.txt; \
         {probe} truncate plain.txt; mv plain.txt dir/plain.txt; rm dir/moved; \
         mkdir gone; rmdir gone"
    );
    let (code, stderr, outcome) = run_script_in(&root, &policy, &[], &script);

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        outcome.approvals.is_empty(),
        "{:?}",
        reported_paths(&outcome)
    );
    assert!(root.join("dir").is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("hardlink")).unwrap(),
        "payload"
    );
    assert!(!root.join("dir/moved").exists());
    assert_eq!(
        std::fs::read_link(root.join("symlink")).unwrap(),
        std::path::Path::new("/tmp/x")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("dir/plain.txt")).unwrap(),
        ""
    );
    assert!(!root.join("gone").exists());

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn an_approved_gitdir_grant_commits_but_cannot_move_hooks_or_config() {
    let root = test_dir("mutation-git-commit").canonicalize().unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main", "."][..],
        &["config", "user.name", "Horizon Test"],
        &["config", "user.email", "horizon-test@example.invalid"],
    ] {
        let mut git = Command::new("git");
        git.args(args).current_dir(&root);
        scrub_git_env(&mut git);
        git.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        assert!(
            git.status().expect("run host git").success(),
            "git {args:?}"
        );
    }
    let git_dir = root.join(".git");
    std::fs::write(root.join("file.txt"), "content").unwrap();
    std::fs::write(root.join("staged"), "#!/bin/sh\n").unwrap();
    let original_config = std::fs::read_to_string(git_dir.join("config")).unwrap();
    let grant = FilesystemGrant {
        path: git_dir.clone(),
        access: FilesystemGrantAccess::ReadWrite,
        scope: FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: vec!["hooks".into(), "config".into()],
    };
    let policy = root_with_excluded_gitdir(&root);

    let (code, stderr, outcome) = run_script_in(
        &root,
        &policy,
        std::slice::from_ref(&grant),
        "export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1; \
         git add file.txt && git commit --quiet -m sandboxed",
    );
    assert_eq!(code, Some(0), "git commit failed: {stderr}");
    assert!(
        outcome.approvals.is_empty(),
        "{:?}",
        reported_paths(&outcome)
    );
    let mut log = Command::new("git");
    log.args(["log", "--format=%s"]).current_dir(&root);
    scrub_git_env(&mut log);
    let log = log.output().expect("run host git log");
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "sandboxed");

    for (script, denied) in [
        (
            "mv staged .git/hooks/pre-commit",
            git_dir.join("hooks/pre-commit"),
        ),
        ("mv .git/config .git/config.bak", git_dir.join("config")),
    ] {
        let (code, stderr, outcome) =
            run_script_in(&root, &policy, std::slice::from_ref(&grant), script);
        assert_ne!(code, Some(0), "`{script}` must fail; stderr: {stderr}");
        assert!(
            reported_paths(&outcome).contains(&denied),
            "`{script}` must report {}; got {:?}",
            denied.display(),
            reported_paths(&outcome)
        );
    }
    assert!(std::fs::symlink_metadata(git_dir.join("hooks/pre-commit")).is_err());
    assert!(!git_dir.join("config.bak").exists());
    assert_eq!(
        std::fs::read_to_string(git_dir.join("config")).unwrap(),
        original_config
    );

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn io_uring_setup_is_refused_while_exclusions_are_active() {
    let root = excluded_gitdir_fixture("mutation-io-uring");
    let policy = root_with_excluded_gitdir(&root);
    let probe = shell_quote(std::path::Path::new(NETWORK_PROBE));
    let (code, stderr, _) = run_script_in(&root, &policy, &[], &format!("{probe} io_uring unused"));

    assert_eq!(code, Some(23), "io_uring_setup must fail; stderr: {stderr}");
    assert!(stderr.contains("PROBE-DENIED"), "{stderr}");

    std::fs::remove_dir_all(root).expect("remove test directory");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn an_x32_abi_syscall_kills_the_sandboxed_process() {
    let root = excluded_gitdir_fixture("x32-abi");
    let hook = root.join(".git/hooks/pre-commit");
    let policy = root_with_excluded_gitdir(&root);
    let probe = shell_quote(std::path::Path::new(NETWORK_PROBE));
    let (code, stderr, _) = run_script_in(
        &root,
        &policy,
        &[],
        &format!("exec {probe} x32-create .git/hooks/pre-commit"),
    );

    assert_eq!(
        code,
        Some(128 + libc::SIGSYS),
        "an x32 openat must kill the process; stderr: {stderr}"
    );
    assert!(std::fs::symlink_metadata(&hook).is_err());

    std::fs::remove_dir_all(root).expect("remove test directory");
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
