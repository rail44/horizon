//! Item 3: drives the remoc 0.16.1 x 0.18.3 cross-version binaries.
//!
//! Run with `cargo test --test cross_version -- --nocapture`.

use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn wait_with_timeout(mut child: Child, timeout: Duration) -> (bool, String, String) {
    let start = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let out = child.wait_with_output().unwrap();
                return (
                    status.success(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                );
            }
            None if start.elapsed() > timeout => {
                child.kill().unwrap();
                let out = child.wait_with_output().unwrap();
                return (
                    false,
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    format!("TIMEOUT; stderr: {}", String::from_utf8_lossy(&out.stderr)),
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn spawn(bin: &str, args: &[&str]) -> Child {
    Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// remoc 0.16.1 <-> 0.18.3 with the same explicit codec (JSON):
/// chmux + base channel + a transported rch::mpsc channel must all work.
#[test]
fn cross_version_same_codec_interoperates() {
    let sock = std::env::temp_dir().join(format!("remoc-spike-xver-{}.sock", std::process::id()));
    let sock = sock.to_str().unwrap();

    let new = spawn(env!("CARGO_BIN_EXE_new_peer"), &[sock]);
    std::thread::sleep(Duration::from_millis(300)); // let it bind
    let old = spawn(env!("CARGO_BIN_EXE_old_peer"), &[sock]);

    let (old_ok, old_out, old_err) = wait_with_timeout(old, Duration::from_secs(20));
    let (new_ok, new_out, new_err) = wait_with_timeout(new, Duration::from_secs(20));

    println!("--- old peer (remoc 0.16.1) ---\n{old_out}{old_err}");
    println!("--- new peer (remoc 0.18.3) ---\n{new_out}{new_err}");

    assert!(old_ok && old_out.contains("PASS"), "old peer failed");
    assert!(new_ok && new_out.contains("PASS"), "new peer failed");
    assert!(new_out.contains("nums count 100 sum 4950"));
}

/// remoc 0.16.1 default codec (JSON) vs remoc 0.18.3 default codec
/// (Postbag): chmux still connects, decoding the base channel item fails.
#[test]
fn cross_version_default_codecs_mismatch() {
    let sock = std::env::temp_dir().join(format!("remoc-spike-xmis-{}.sock", std::process::id()));
    let sock = sock.to_str().unwrap();

    let new = spawn(env!("CARGO_BIN_EXE_new_peer"), &[sock, "--postbag"]);
    std::thread::sleep(Duration::from_millis(300));
    let old = spawn(env!("CARGO_BIN_EXE_old_peer"), &[sock, "--no-response"]);

    let (_, old_out, old_err) = wait_with_timeout(old, Duration::from_secs(20));
    let (new_ok, new_out, new_err) = wait_with_timeout(new, Duration::from_secs(20));

    println!("--- old peer (remoc 0.16.1, JSON) ---\n{old_out}{old_err}");
    println!("--- new peer (remoc 0.18.3, Postbag) ---\n{new_out}{new_err}");

    assert!(
        new_ok,
        "new peer crashed instead of reporting the decode error"
    );
    assert!(
        new_out.contains("chmux connection established"),
        "chmux layer should connect regardless of codec"
    );
    assert!(
        new_out.contains("EXPECTED-MISMATCH-ERROR"),
        "expected a deserialization error on the base channel"
    );
}
