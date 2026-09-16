//! Run the recovery fixtures with inherited daemon overrides pointing at a
//! harmless recorder. A successful recovery must never launch that program.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct TestProcess(Child);

impl Drop for TestProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn stub_recovery_never_launches_inherited_daemon_binaries() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hzn-no-spawn-{}", uuid::Uuid::new_v4().simple())),
    );
    fs::create_dir_all(&scratch.0).unwrap();
    let recorder = scratch.0.join("daemon-recorder");
    fs::write(
        &recorder,
        "#!/bin/sh\nprintf 'spawned\\n' >> \"$HORIZON_TEST_SPAWN_RECORD\"\n",
    )
    .unwrap();
    fs::set_permissions(&recorder, fs::Permissions::from_mode(0o700)).unwrap();
    let attempts = scratch.0.join("spawned.txt");

    for case in [
        "a_second_generation_mismatch_after_recovery_goes_fatal_instead_of_looping",
        "a_range_rejecting_remoc_daemon_is_drained_via_rtc_and_the_respawn_adopted",
        "a_range_rejecting_terminald_is_drained_via_rtc_and_the_respawn_adopted",
    ] {
        let output_path = scratch.0.join("test-output.txt");
        let output = fs::File::create(&output_path).unwrap();
        let mut child = TestProcess(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("runtime::tests::{case}"), "--nocapture"])
                // Configure only the child: parallel cargo-test cases must
                // not race over this process's environment.
                .env("HORIZON_AGENTD_BINARY", &recorder)
                .env("HORIZON_TERMINALD_BINARY", &recorder)
                .env("HORIZON_LOGD_BINARY", &recorder)
                .env("HORIZON_TEST_SPAWN_RECORD", &attempts)
                .env("HORIZON_CONFIG", scratch.0.join("absent-config.toml"))
                .env("HORIZON_AGENT_EVENT_LOG", scratch.0.join("events.jsonl"))
                .env("HORIZON_AGENT_STATE_DB", scratch.0.join("state.duckdb"))
                .stdout(Stdio::from(output.try_clone().unwrap()))
                .stderr(Stdio::from(output))
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "{case} did not finish");
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = fs::read_to_string(&output_path).unwrap();
        assert!(status.success(), "{case} failed: {output}");
        assert!(output.contains("1 passed"), "{case} was not run: {output}");
        assert!(
            !attempts.exists(),
            "{case} launched a daemon while reconnecting to its in-process stub"
        );
    }
}
