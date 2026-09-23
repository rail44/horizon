//! Operating-system process-tree termination shared by both bash runners.

#[cfg(target_os = "linux")]
use std::collections::{HashMap, VecDeque};

/// A handle capable of terminating a running bash child's entire process
/// tree (`docs/agent-tools-design.md`, "Bash Semantics"). The child is
/// spawned with `process_group(0)` on unix, which makes its pid double as
/// its own pgid, so signalling `-pid` reaches every descendant still in
/// that group (a pipeline, a backgrounded job — not just the direct
/// `bash` child). Descendants that escaped the group via
/// `setsid`/`setpgid` survive the group signal; [`kill_process_tree`]
/// additionally walks `/proc` to find and kill those individually (issue
/// 017 — a `git commit` whose pre-commit hook's child daemonised would
/// otherwise survive the timeout kill and land the commit anyway).
#[derive(Clone, Copy, Debug)]
pub(super) struct KillHandle {
    pid: u32,
}

impl KillHandle {
    pub(super) fn new(pid: u32) -> Self {
        Self { pid }
    }

    pub(super) fn kill(self) {
        kill_process_tree(self.pid);
    }
}

/// Kills `root_pid`'s entire process tree: first signals the process group
/// (reaching the root and every descendant still in it), then — on Linux —
/// walks `/proc` to find descendants that escaped the group via
/// `setsid`/`setpgid` and kills them individually by pid. The `/proc`
/// snapshot is taken *before* the group kill so the PPID chain is still
/// intact: once an ancestor is SIGKILL'd, its orphaned children are
/// reparented to PID 1 (or a subreaper) and become untraceable by PPID.
///
/// Shared by both the unsandboxed (tokio) and sandboxed (helper) bash
/// paths — in both, the registered pid is the process-group leader whose
/// pid doubles as the pgid (`process_group(0)` at spawn).
#[cfg(unix)]
pub(super) fn kill_process_tree(root_pid: u32) {
    #[cfg(target_os = "linux")]
    let escaped = enumerate_descendants(root_pid);
    #[cfg(not(target_os = "linux"))]
    let escaped: Vec<u32> = Vec::new();

    kill_process_group(root_pid);

    for pid in &escaped {
        // SAFETY: best-effort; ESRCH (already dead) is a harmless no-op.
        // There is a negligible pid-reuse window between the snapshot and
        // this kill, but it is the same window the group kill itself has.
        unsafe {
            libc::kill(*pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
pub(super) fn kill_process_tree(root_pid: u32) {
    let _ = root_pid;
}

/// Signals the whole process group whose pgid is `pid`. Reaches the root
/// and every descendant still in the group, but not descendants that left
/// the group via `setsid`/`setpgid` — those are handled by the `/proc` walk
/// in [`kill_process_tree`].
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: `libc::kill` has no memory-safety preconditions to uphold; a
    // negative pid targets the whole process group rather than a single
    // pid. Best-effort: ESRCH (the process already exited) is a normal,
    // harmless outcome here, not something to propagate.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(pid: u32) {
    // No portable process-group signal outside unix. Best effort only: this
    // reaches the direct `bash` child but not further descendants it may
    // have spawned.
    let _ = pid;
}

/// Snapshots every descendant of `root_pid` by walking `/proc`: reads each
/// process's `stat` file for its PPID, builds a child map, and BFS-collects
/// all descendants. Only processes whose PPID chain is intact at snapshot
/// time are found — a process that already daemonised (setsid + parent
/// exited, reparented to PID 1) is invisible. In practice the root is still
/// alive at this point (the caller has not killed anything yet), so the
/// tree is intact.
#[cfg(target_os = "linux")]
fn enumerate_descendants(root_pid: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(ppid) = read_proc_ppid(pid) {
            children.entry(ppid).or_default().push(pid);
        }
    }

    let mut descendants = Vec::new();
    let mut queue = VecDeque::new();
    if let Some(root_children) = children.get(&root_pid) {
        queue.extend(root_children.iter().copied());
    }
    while let Some(pid) = queue.pop_front() {
        descendants.push(pid);
        if let Some(grandchildren) = children.get(&pid) {
            queue.extend(grandchildren.iter().copied());
        }
    }
    descendants
}

/// Reads the PPID field from `/proc/<pid>/stat`. The `comm` field (second
/// field) is enclosed in parentheses and may contain spaces, so parsing
/// skips past the last `)` before reading the fixed-width fields that
/// follow: `state ppid pgrp sess ...`.
#[cfg(target_os = "linux")]
fn read_proc_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rfind(')')?;
    // After the last ')': state ppid pgrp sess ...
    rest.checked_add(1)
        .map(|start| &stat[start..])
        .and_then(|fields| fields.split_whitespace().nth(1))
        .and_then(|ppid| ppid.parse().ok())
}
