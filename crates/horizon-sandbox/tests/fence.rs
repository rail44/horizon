//! `FenceRuntime` against the real `fence` binary, found on `PATH`. These
//! start a real sandbox (bubblewrap on Linux), so they cannot run inside
//! one: the `sandboxed` nextest profile skips this binary.

use horizon_sandbox::runtime::{CONTAINED_ENV, Containment, FenceRuntime, SandboxRuntime};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

fn fence() -> FenceRuntime {
    let path = std::env::var_os("PATH").expect("PATH is set");
    let binary = std::env::split_paths(&path)
        .map(|dir| dir.join("fence"))
        .find(|candidate| candidate.is_file())
        .expect("these tests need Fence on PATH (https://github.com/fencesandbox/fence)");
    let fence = FenceRuntime::new(binary);
    fence.probe().expect("Fence can contain a command here");
    fence
}

struct Run {
    output: Output,
    stdout: String,
    stderr: String,
}

fn run(containment: &Containment, cwd: &Path, script: &str) -> Run {
    let fence = fence();
    let scratch = tempfile::tempdir().unwrap();
    let mut contained = fence.contain(script, containment, scratch.path()).unwrap();
    let output = contained
        .command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("run fence");
    drop(contained.keep_alive);
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        output,
    }
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

#[test]
fn writes_reach_only_the_writable_trees() {
    let (_workspace, root) = workspace();
    let (_outside, outside) = workspace();
    let containment = Containment {
        writable: vec![root.clone()],
        ..Containment::default()
    };

    let run = run(
        &containment,
        &root,
        &format!(
            "touch inside && echo inside-ok; touch {}/outside; echo outside=$?",
            outside.display()
        ),
    );

    assert!(run.stdout.contains("inside-ok"), "{}", run.stderr);
    assert!(run.stdout.contains("outside=1"), "{}", run.stdout);
    assert!(root.join("inside").exists());
    assert!(!outside.join("outside").exists());
}

#[test]
fn a_read_only_path_inside_a_writable_tree_stays_read_only() {
    let (_workspace, root) = workspace();
    std::fs::create_dir(root.join("kept")).unwrap();
    let containment = Containment {
        writable: vec![root.clone()],
        read_only: vec![root.join("kept")],
        ..Containment::default()
    };

    let run = run(
        &containment,
        &root,
        "touch kept/x; echo kept=$?; mkdir -p target && echo target-ok",
    );

    assert!(run.stdout.contains("kept=1"), "{}", run.stdout);
    assert!(run.stdout.contains("target-ok"), "{}", run.stderr);
    assert!(!root.join("kept/x").exists());
}

#[test]
fn nothing_writable_writes_nothing() {
    let (_workspace, root) = workspace();

    let run = run(&Containment::default(), &root, "touch x; echo x=$?");

    assert!(run.stdout.contains("x=1"), "{}", run.stdout);
    assert!(!root.join("x").exists());
}

#[test]
fn a_workspace_fence_json_does_not_widen_the_call() {
    let (_workspace, root) = workspace();
    let (_outside, outside) = workspace();
    std::fs::write(
        root.join("fence.json"),
        format!(
            r#"{{"filesystem": {{"allowWrite": ["{}"]}}}}"#,
            outside.display()
        ),
    )
    .unwrap();
    let containment = Containment {
        writable: vec![root.clone()],
        ..Containment::default()
    };

    run(
        &containment,
        &root,
        &format!("touch {}/x", outside.display()),
    );

    assert!(!outside.join("x").exists());
}

#[test]
fn a_host_off_the_allowlist_is_refused_and_named() {
    let (_workspace, root) = workspace();
    let containment = Containment {
        writable: vec![root.clone()],
        ..Containment::default()
    };

    let run = run(
        &containment,
        &root,
        "curl -sS -o /dev/null --max-time 10 https://example.com; echo curl=$?",
    );

    assert!(!run.stdout.contains("curl=0"), "{}", run.stdout);
    assert_eq!(fence().refused_hosts(&run.stderr), ["example.com"]);
}

#[test]
fn the_contained_command_is_told_it_is_contained() {
    let (_workspace, root) = workspace();

    let run = run(
        &Containment::default(),
        &root,
        &format!("echo ${CONTAINED_ENV}"),
    );

    assert_eq!(run.stdout.trim(), "fence", "{}", run.stderr);
    assert!(run.output.status.success());
}

#[test]
fn ending_the_process_group_ends_the_contained_command() {
    use std::os::unix::process::CommandExt;
    let (_workspace, root) = workspace();
    let fence = fence();
    let scratch = tempfile::tempdir().unwrap();
    let marker = root.join("still-running");
    let mut contained = fence
        .contain(
            &format!("sleep 3; touch {}", marker.display()),
            &Containment {
                writable: vec![root.clone()],
                ..Containment::default()
            },
            scratch.path(),
        )
        .unwrap();
    let mut child = contained
        .command
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));

    let group = child.id() as i32;
    // SAFETY: signalling a process group this test created.
    assert_eq!(unsafe { libc::kill(-group, libc::SIGKILL) }, 0);
    child.wait().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(4));

    assert!(!marker.exists(), "the contained command outlived its group");
}

#[test]
fn a_temp_dir_is_the_calls_tmpdir_and_keeps_its_files_across_calls() {
    let (_workspace, root) = workspace();
    let (_temp, temp) = workspace();
    let containment = Containment {
        temp_dir: Some(temp.clone()),
        ..Containment::default()
    };

    let first = run(
        &containment,
        &root,
        "echo kept > \"$TMPDIR/note\" && echo \"$TMPDIR\"",
    );
    let second = run(&containment, &root, "cat \"$TMPDIR/note\"");

    assert_eq!(
        first.stdout.trim(),
        temp.display().to_string(),
        "{}",
        first.stderr
    );
    assert_eq!(second.stdout.trim(), "kept", "{}", second.stderr);
}
