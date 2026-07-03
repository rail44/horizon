use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::agent::contract::ToolCallId;

/// A handle capable of terminating a running bash child's entire process
/// group (`docs/agent-tools-design.md`, "Bash Semantics": "Cancelling a turn
/// kills the process group of any in-flight command"). The child is spawned
/// with `process_group(0)` on unix, which makes its pid double as its own
/// pgid, so signalling `-pid` reaches every process the command spawned too
/// (e.g. a pipeline, or a backgrounded job) — not just the direct `bash`
/// child.
#[derive(Clone, Copy, Debug)]
pub(super) struct KillHandle {
    pid: u32,
}

impl KillHandle {
    pub(super) fn new(pid: u32) -> Self {
        Self { pid }
    }

    fn kill(self) {
        kill_process_group(self.pid);
    }
}

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

type Table = Mutex<HashMap<ToolCallId, KillHandle>>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock(table: &'static Table) -> MutexGuard<'static, HashMap<ToolCallId, KillHandle>> {
    table
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register(call_id: ToolCallId, handle: KillHandle) {
    lock(table()).insert(call_id, handle);
}

fn unregister(call_id: &ToolCallId) {
    lock(table()).remove(call_id);
}

/// Kills the running child registered for `call_id`, if any, and removes it
/// from the registry. Returns whether a child was found — a miss is a
/// harmless no-op to callers (the call may already have finished on its
/// own, or may not be a bash call at all; `agent::tools::processing` calls
/// this unconditionally for every provider-originated `ToolCallFinished`,
/// e.g. the synthetic one a cancelled turn produces).
pub(crate) fn kill(call_id: &ToolCallId) -> bool {
    let handle = lock(table()).remove(call_id);
    let found = handle.is_some();
    if let Some(handle) = handle {
        handle.kill();
    }
    found
}

/// RAII registration for a running bash child: registers on construction,
/// unregisters on drop (whichever path drops it — normal completion, an
/// error return, or an unwind) so the registry never accumulates entries
/// for calls that are no longer running.
pub(super) struct RegistryGuard {
    call_id: ToolCallId,
}

impl RegistryGuard {
    pub(super) fn new(call_id: ToolCallId, pid: u32) -> Self {
        register(call_id.clone(), KillHandle::new(pid));
        Self { call_id }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        unregister(&self.call_id);
    }
}

#[cfg(test)]
pub(super) fn is_registered(call_id: &ToolCallId) -> bool {
    lock(table()).contains_key(call_id)
}
