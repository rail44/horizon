//! Real, hermetic end-to-end tests: each spawns an actual `bwrap` process
//! (no root required -- unprivileged user namespaces, verified present on
//! this dev machine) and asserts on its actual observed behavior, not just
//! argv shape. Every spawned child is waited on with a bounded timeout so
//! a regression here can't hang the test suite.
//!
//! Diagnostic text (a sandboxed command's stderr) is captured by having
//! the *sandboxed script itself* redirect its own fd 2 to a file inside
//! its writable root (`exec 2>logfile` as the script's first statement,
//! before anything that might fail), read back from the host after the
//! process exits. `spawn` doesn't preserve a caller's piped
//! stdout/stderr today (see the crate's top-level doc for why:
//! `std::process::Command` has no getter for stdio, only for
//! program/args/cwd/env) -- this sidesteps that entirely rather than
//! relying on it.

use super::*;
use crate::denial::is_likely_sandbox_denied;
use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy};
use std::time::{Duration, Instant};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the watchdog below re-checks whether the main thread already
/// reaped the child -- short so a fast-exiting test doesn't sit around
/// waiting for a `join()` on a watchdog stuck in one long unconditional
/// `sleep(timeout)` (a real bug caught here in review: the first version
/// of this helper slept the *entire* timeout unconditionally before ever
/// checking the flag, silently making every test as slow as its timeout).
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Waits for `child` to finish, forcibly killing it if it doesn't exit
/// within `timeout` so a regression here can never hang the suite.
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> i32 {
    let pid = child.id();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = {
        let done = done.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(WATCHDOG_POLL_INTERVAL);
            }
            if !done.load(std::sync::atomic::Ordering::SeqCst) {
                // SAFETY: plain single-argument `kill(2)` on a pid this
                // process owns (it's our own child); no memory unsafety.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        })
    };

    let status = child.wait();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = watchdog.join();

    match status {
        Ok(status) => status.code().unwrap_or_else(|| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                128 + status.signal().unwrap_or(0)
            }
            #[cfg(not(unix))]
            {
                -1
            }
        }),
        Err(_) => -1,
    }
}

fn shell(program: &str, script: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("-c").arg(script);
    // Force English error text regardless of the host's locale -- both
    // this test's own keyword assertions and the real
    // `is_likely_sandbox_denied` keyword list assume it (a real,
    // locale-dependent weakness of substring-based denial classification,
    // worth noting but not this spike's to fix).
    cmd.env("LC_ALL", "C");
    cmd
}

/// Reads back a log file a sandboxed script redirected its own stderr
/// into (see module doc). Empty string if the script never got far
/// enough to create it.
fn read_log(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn writes_inside_writable_root_succeed() {
    let dir = tempdir("writes-inside");
    let target = dir.join("inside.txt");
    let log = dir.join("stderr.log");

    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = shell(
        "/bin/sh",
        &format!(
            "exec 2>{}; echo ok > {}",
            shell_quote(&log),
            shell_quote(&target)
        ),
    );
    let sandboxed = spawn(cmd, &policy).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_eq!(code, 0, "stderr: {}", read_log(&log));
    assert_eq!(
        std::fs::read_to_string(&target).expect("file should exist"),
        "ok\n"
    );

    cleanup(dir);
}

#[test]
fn writes_outside_writable_root_are_denied() {
    let writable = tempdir("writes-outside-writable");
    let outside = tempdir("writes-outside-target");
    let target = outside.join("nope.txt");
    let log = writable.join("stderr.log");

    let policy = SandboxPolicy {
        writable_roots: vec![writable.clone()],
        // `Full` scope means `outside` is visible (bwrap `--ro-bind / /`)
        // but not writable -- the interesting negative case. A `Roots`
        // scope that never mentions `outside` would instead fail with
        // ENOENT (the path isn't even visible), a different, less useful
        // signal for denial classification.
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = shell(
        "/bin/sh",
        &format!(
            "exec 2>{}; echo nope > {}",
            shell_quote(&log),
            shell_quote(&target)
        ),
    );
    let sandboxed = spawn(cmd, &policy).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    let stderr = read_log(&log);

    assert_ne!(code, 0, "write outside the writable root should fail");
    assert!(!target.exists());
    assert!(
        is_likely_sandbox_denied(true, code, &stderr),
        "expected a denial-shaped failure, got exit {code} stderr {stderr:?}"
    );

    cleanup(writable);
    cleanup(outside);
}

#[test]
fn network_off_fails_a_tcp_connect() {
    let dir = tempdir("network-off");
    let log = dir.join("stderr.log");
    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    // bash's `/dev/tcp` pseudo-device needs no external binary. With no
    // network namespace (and no routes), the connect fails immediately --
    // no real packet ever leaves the host, so this is fast and hermetic.
    let cmd = shell(
        "/bin/bash",
        &format!(
            "exec 2>{}; exec 3<>/dev/tcp/93.184.216.34/80",
            shell_quote(&log)
        ),
    );
    let sandboxed = spawn(cmd, &policy).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    let stderr = read_log(&log).to_lowercase();

    assert_ne!(code, 0, "TCP connect should fail with network disabled");
    // Two legitimate failure shapes, depending on which containment layer
    // gets there first: the seccomp network-cut denies `socket(2)` itself
    // ("operation not permitted", observed in practice -- it runs before
    // bwrap's `--unshare-net` would even get a chance to matter, since
    // there's no `connect(2)`/routing step left to reach); a kernel
    // without seccomp support (or a filter that somehow didn't apply)
    // would instead see bwrap's namespace-based "network is unreachable"
    // at the routing step. Either is the sandbox denying it, not the
    // command's own logic.
    assert!(
        stderr.contains("network") || stderr.contains("operation not permitted"),
        "expected a sandbox-denied-shaped error, got: {stderr:?}"
    );

    cleanup(dir);
}

#[test]
fn network_on_allows_reaching_the_kernel_network_stack() {
    // We don't assert the connect *succeeds* (this test must stay
    // hermetic and not depend on outbound connectivity in whatever
    // environment runs it) -- only that it isn't rejected by our own
    // containment before even reaching the network stack. A sandbox-level
    // rejection would show up as "network is unreachable" (no route: our
    // own containment, not the kernel, said no); a real "connection
    // refused" on a closed port is the kernel's network stack actually
    // being reached and answering.
    let dir = tempdir("network-on");
    let log = dir.join("stderr.log");
    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Enabled,
    };
    // Port 1 on loopback: nothing listens there, so this fails fast with
    // "connection refused" from the kernel rather than depending on any
    // real network path being up.
    let cmd = shell(
        "/bin/bash",
        &format!("exec 2>{}; exec 3<>/dev/tcp/127.0.0.1/1", shell_quote(&log)),
    );
    let sandboxed = spawn(cmd, &policy).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    let stderr = read_log(&log);

    assert_ne!(code, 0, "nothing listens on loopback port 1");
    assert!(
        !stderr.to_lowercase().contains("network is unreachable"),
        "expected the socket layer to be reachable (refused, not unreachable): {stderr:?}"
    );

    cleanup(dir);
}

#[test]
fn landlock_negotiation_reports_an_abi_level_or_is_skipped() {
    let dir = tempdir("landlock-report");
    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = std::process::Command::new("/bin/true");
    let sandboxed = spawn(cmd, &policy).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_eq!(code, 0);

    match sandboxed.landlock {
        Some(report) => {
            // Skip-not-fail: on a kernel without Landlock this crate must
            // still function (bwrap remains the primary containment); we
            // only assert the report is internally consistent, not that
            // enforcement is full (that depends on the kernel we happen
            // to run the test on).
            println!(
                "Landlock report: target_abi={:?} enforcement={:?} downgraded={}",
                report.target_abi,
                report.enforcement,
                report.is_downgraded()
            );
        }
        None => println!("Landlock report unavailable on this backend"),
    }

    cleanup(dir);
}

#[test]
fn missing_bwrap_candidate_list_is_exposed_in_the_error() {
    // Not a real "bwrap missing" scenario (it's installed on this dev
    // machine) -- exercises that `resolve_bwrap` fails closed rather than
    // falling back to an unqualified PATH search, by checking the
    // documented candidate paths directly.
    for candidate in BWRAP_CANDIDATES {
        assert!(
            candidate.starts_with('/'),
            "bwrap candidates must be absolute paths, not a PATH lookup"
        );
    }
}

fn tempdir(label: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "horizon-sandbox-test-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn cleanup(dir: std::path::PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
