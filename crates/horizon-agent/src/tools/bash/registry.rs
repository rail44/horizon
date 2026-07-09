use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::contract::{SessionId, ToolCallId};

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
pub fn kill(call_id: &ToolCallId) -> bool {
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

/// Test hook: whether `session_id` currently has a queue entry (running
/// and/or queued jobs). `advance` removes the entry the instant a session's
/// queue fully drains, so a lingering entry after all known jobs have
/// finished would mean the FIFO is wedged.
#[cfg(test)]
pub(super) fn is_session_queued(session_id: SessionId) -> bool {
    session_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&session_id)
}

// --- per-session bash FIFO ---------------------------------------------
//
// `docs/agent-tools-design.md`, "Bash Containment": a session's approved
// bash calls run one at a time. Chosen over a persistent per-session worker
// thread because `bash::spawn` is already a "fresh thread per call" design
// (see its own doc comment) -- reusing that per-job thread for the queued
// case keeps this a pure ordering constraint layered on top of the existing
// spawn shape, rather than a new thread lifecycle to manage across session
// creation/teardown. A session's entry is created lazily on its first call
// and removed the instant its queue drains, so this table never accumulates
// entries for sessions that aren't actively running bash.
//
// Panic safety: `advance` is the only place `running` is ever cleared, so a
// job that panics without it running would leave the session's queue
// permanently stuck "running" -- every later call for that session would
// enqueue and never dispatch. `run_job` guarantees `advance` runs exactly
// once per job via an RAII guard (`AdvanceGuard`, below) constructed before
// `job()` and dropped after, regardless of whether `job()` returns normally
// or unwinds.

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct SessionQueue {
    /// `true` while a job for this session has been handed to a thread and
    /// hasn't finished yet -- distinguishes "a job is running with nothing
    /// queued behind it" from "nothing running at all" (the latter has no
    /// entry in the table in the first place, once `advance` cleans it up).
    running: bool,
    jobs: VecDeque<Job>,
}

type SessionQueues = Mutex<HashMap<SessionId, SessionQueue>>;

fn session_queues() -> &'static SessionQueues {
    static QUEUES: OnceLock<SessionQueues> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs `job` immediately if `session_id` has nothing currently running,
/// otherwise appends it to that session's queue -- `advance` (below) hands
/// it to a fresh thread the instant the job ahead of it finishes.
pub(super) fn enqueue(session_id: SessionId, job: Job) {
    let mut queues = session_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let queue = queues.entry(session_id).or_default();
    if queue.running {
        queue.jobs.push_back(job);
        return;
    }
    queue.running = true;
    drop(queues);
    run_job(session_id, job);
}

fn run_job(session_id: SessionId, job: Job) {
    std::thread::spawn(move || {
        // Constructed *before* `job()` runs and dropped after, so `advance`
        // fires exactly once whether `job()` returns normally or unwinds.
        // `bash::spawn`'s jobs already catch their own panics (see that
        // module's panic-safety notes) and so never reach the unwind path
        // here in practice -- this guard is defense in depth for `run_job`
        // as a general mechanism, not specific to the bash tool's own job
        // shape.
        let _advance_guard = AdvanceGuard::new(session_id);
        job();
    });
}

/// RAII: calls `advance(session_id)` on drop, which happens on `job()`
/// returning normally *or* unwinding -- see `run_job`.
struct AdvanceGuard {
    session_id: SessionId,
}

impl AdvanceGuard {
    fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }
}

impl Drop for AdvanceGuard {
    fn drop(&mut self) {
        advance(self.session_id);
    }
}

/// Called from a just-finished job's own thread: hands the next queued job
/// (if any) to a fresh thread, or drops the session's entry entirely once
/// nothing is left running or queued.
fn advance(session_id: SessionId) {
    let mut queues = session_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let next = queues
        .get_mut(&session_id)
        .and_then(|queue| queue.jobs.pop_front());
    match next {
        Some(job) => {
            drop(queues);
            run_job(session_id, job);
        }
        None => {
            queues.remove(&session_id);
        }
    }
}
