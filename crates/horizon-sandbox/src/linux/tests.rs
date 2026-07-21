//! Real, hermetic end-to-end tests: each spawns an actual process under a
//! real nono/Landlock sandbox (no root required -- unprivileged Landlock,
//! verified present on this dev machine) and asserts on its actual
//! observed behavior, not just `CapabilitySet` shape (see `caps::tests`
//! for that level). Every spawned child is waited on with a bounded
//! timeout so a regression here can't hang the test suite.
//!
//! Diagnostic text (a sandboxed command's stderr) is captured by having
//! the *sandboxed script itself* redirect its own fd 2 to a file inside
//! its writable root (`exec 2>logfile` as the script's first statement,
//! before anything that might fail), read back from the host after the
//! process exits -- simpler than piping for these tests' purposes, even
//! though `spawn` can now carry a caller's piped stdio too (see
//! `SandboxStdio`, and `spawn_preserves_piped_stdout` below for direct
//! proof of that).

use super::*;
use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy, SandboxStdio};
use std::io::Read;
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
    // Keep diagnostic output stable when an assertion includes stderr.
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
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
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
        // `Full` scope means `outside` is visible (nono's `/` Read grant)
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
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_ne!(code, 0, "write outside the writable root should fail");
    assert!(!target.exists());

    cleanup(writable);
    cleanup(outside);
}

/// TMPDIR parity (`docs/roadmap.md`'s backlog-60 entry): the one
/// deliberate behavior change from the old bwrap backend. bwrap gave a
/// private `--tmpfs /tmp`, so a sandboxed write to a literal `/tmp/<name>`
/// always succeeded (landing in that private overlay, torn down with the
/// sandbox). nono has no mount namespace, so there is no such overlay to
/// substitute -- a literal `/tmp` write is now denied outright unless the
/// caller's own `writable_roots` happens to cover it (which callers
/// should not do; see `linux::spawn`'s TMPDIR-parity comment). This is
/// the regression test proving that denial is real and leaves no trace
/// on the host's actual `/tmp`, not a silent no-op.
#[test]
fn literal_tmp_write_is_denied_without_a_matching_writable_root() {
    let workspace = tempdir("literal-tmp-denied-workspace");
    let log = workspace.join("stderr.log");
    // A name unique enough that a pre-existing file at this exact host
    // path would be an astonishing coincidence, plus a defensive
    // pre-clean and a best-effort post-clean, so a hypothetical
    // regression here can't leave a stray file behind on the real
    // machine running this test.
    let marker = format!(
        "horizon-sandbox-test-tmp-denied-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let host_target = std::path::Path::new("/tmp").join(&marker);
    let _ = std::fs::remove_file(&host_target);

    let policy = SandboxPolicy {
        // Deliberately not `/tmp` or anything under it.
        writable_roots: vec![workspace.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = shell(
        "/bin/sh",
        &format!(
            "exec 2>{}; echo leaked > {}",
            shell_quote(&log),
            shell_quote(&host_target)
        ),
    );
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_ne!(code, 0, "a literal /tmp write should be denied");
    assert!(
        !host_target.exists(),
        "the write must never actually land on the host's real /tmp"
    );

    cleanup(workspace);
    let _ = std::fs::remove_file(&host_target);
}

/// The other half of TMPDIR parity: `spawn` auto-provisions
/// `<writable_root>/.horizon-sandbox-tmp` and injects `TMPDIR` when the
/// caller hasn't already set one, so TMPDIR-respecting tools (`mktemp`
/// here, standing in for the many language runtimes that behave the
/// same way) get private, writable scratch space without ever touching
/// the host's real `/tmp` -- functionally replacing bwrap's private
/// tmpfs. `TMPDIR` is explicitly cleared on this test's own `Command` so
/// the result doesn't depend on whether the test process's own ambient
/// environment happens to have `TMPDIR` set.
#[test]
fn tmpdir_is_provisioned_when_the_caller_hasnt_set_one_and_mktemp_respects_it() {
    let workspace = tempdir("tmpdir-provision-workspace");
    let log = workspace.join("stderr.log");
    let readback = workspace.join("mktemp-path.txt");

    let policy = SandboxPolicy {
        writable_roots: vec![workspace.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let mut cmd = shell(
        "/bin/sh",
        &format!(
            "exec 2>{}; mktemp > {}",
            shell_quote(&log),
            shell_quote(&readback)
        ),
    );
    cmd.env_remove("TMPDIR");
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_eq!(
        code,
        0,
        "mktemp under the provisioned TMPDIR should succeed, stderr: {}",
        read_log(&log)
    );

    let created_path = std::fs::read_to_string(&readback).expect("readback file should exist");
    let created_path = created_path.trim();
    let expected_scratch = workspace
        .join(crate::SCRATCH_DIR_NAME)
        .canonicalize()
        .expect("spawn should have created the scratch dir");
    assert!(
        std::path::Path::new(created_path).starts_with(&expected_scratch),
        "mktemp's output {created_path:?} should land under the provisioned scratch dir \
         {expected_scratch:?}"
    );
    assert!(
        std::path::Path::new(created_path).exists(),
        "the mktemp'd file must be a real host path"
    );

    cleanup(workspace);
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
    // bash's `/dev/tcp` pseudo-device needs no external binary. With
    // Landlock denying the connect (or, on an older kernel ABI without
    // Landlock network support, nono's automatic seccomp fallback denying
    // `socket(2)` itself), no real packet ever leaves the host, so this
    // is fast and hermetic.
    let cmd = shell(
        "/bin/bash",
        &format!(
            "exec 2>{}; exec 3<>/dev/tcp/93.184.216.34/80",
            shell_quote(&log)
        ),
    );
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    let stderr = read_log(&log).to_lowercase();

    assert_ne!(code, 0, "TCP connect should fail with network disabled");
    // Landlock denies with EACCES ("permission denied"); a seccomp
    // fallback (older ABI) denies `socket(2)` with EPERM ("operation not
    // permitted"); a kernel where neither layer engaged would instead see
    // a plain "network is unreachable" at the routing step. Any of these
    // is the sandbox denying it, not the command's own logic.
    assert!(
        stderr.contains("network")
            || stderr.contains("operation not permitted")
            || stderr.contains("permission denied"),
        "expected a sandbox-denied-shaped error, got: {stderr:?}"
    );

    cleanup(dir);
}

/// Signal scoping (`SignalMode::AllowSameSandbox`) is a new containment
/// win over the old bwrap+seccompiler backend, which had no equivalent at
/// all (`docs/roadmap.md`'s backlog-60 entry). A same-uid process outside
/// the sandbox must survive a real `kill(2)` sent from inside it -- not
/// just a syscall that *reports* denial, but one the target genuinely
/// never received.
#[test]
fn external_signal_is_denied_and_the_decoy_survives() {
    let dir = tempdir("signal-external-denied");
    let log = dir.join("stderr.log");

    let mut decoy = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn decoy process");
    let decoy_pid = decoy.id();

    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = shell(
        "/bin/sh",
        &format!("exec 2>{}; kill -TERM {}", shell_quote(&log), decoy_pid),
    );
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);
    assert_ne!(
        code, 0,
        "signaling an external same-uid process should be denied"
    );

    // The definitive check: the decoy actually never received the
    // signal, not just that the syscall reported an error.
    std::thread::sleep(Duration::from_millis(200));
    match decoy.try_wait() {
        Ok(None) => {} // still alive -- signal scoping held
        Ok(Some(status)) => panic!(
            "CONTAINMENT BREACH: decoy process exited ({status:?}) -- the sandboxed \
             command's kill(2) reached a process outside its sandbox"
        ),
        Err(e) => panic!("try_wait on decoy failed: {e}"),
    }
    let _ = decoy.kill();
    let _ = decoy.wait();

    cleanup(dir);
}

/// The complementary case to `external_signal_is_denied_and_the_decoy_survives`:
/// `SignalMode::AllowSameSandbox` must not block a sandboxed process from
/// signaling its own children.
#[test]
fn sandboxed_process_can_still_signal_its_own_child() {
    let dir = tempdir("signal-own-child-allowed");
    let log = dir.join("stderr.log");

    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    // `wait "$child"` reports the terminated child's own exit status
    // (128 + SIGTERM) once `kill` actually reaches it -- a real syscall
    // outcome, not a timeout: if signaling were denied, `sleep 5` would
    // instead run to its own natural completion (exit 0) before `wait`
    // returns.
    let cmd = shell(
        "/bin/bash",
        &format!(
            "exec 2>{}; sleep 5 & child=$!; sleep 0.2; kill -TERM \"$child\"; wait \"$child\"",
            shell_quote(&log)
        ),
    );
    let sandboxed = spawn(cmd, &policy, SandboxStdio::inherit()).expect("spawn should succeed");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);

    assert_eq!(
        code,
        128 + libc::SIGTERM,
        "the sandboxed process should be able to SIGTERM its own child, stderr: {}",
        read_log(&log)
    );

    cleanup(dir);
}

#[test]
fn spawn_preserves_piped_stdout() {
    let dir = tempdir("piped-stdout");
    let policy = SandboxPolicy {
        writable_roots: vec![dir.clone()],
        readable_scope: ReadableScope::Full,
        network: NetworkPolicy::Disabled,
    };
    let cmd = shell("/bin/sh", "echo from-inside-the-sandbox");
    let mut sandboxed =
        spawn(cmd, &policy, SandboxStdio::piped_output()).expect("spawn should succeed");
    let mut stdout = sandboxed
        .child
        .stdout
        .take()
        .expect("stdout should be piped");
    let mut captured = String::new();
    stdout
        .read_to_string(&mut captured)
        .expect("read piped stdout");
    let code = wait_with_timeout(sandboxed.child, TEST_TIMEOUT);

    assert_eq!(code, 0);
    assert_eq!(captured, "from-inside-the-sandbox\n");

    cleanup(dir);
}

#[test]
fn is_available_agrees_with_detect_abi() {
    assert_eq!(super::is_available(), nono::Sandbox::detect_abi().is_ok());
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
