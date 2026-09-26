//! Own each bash job from enqueue through completion, including cancellation.
use std::collections::{hash_map::Entry, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use super::process::KillHandle;
use crate::contract::SessionId;
#[cfg(test)]
use crate::contract::ToolCallId;

#[derive(Default)]
struct ExecutionState {
    cancelled: bool,
    process: Option<KillHandle>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) struct Registration {
    work: super::super::background::Registration,
    state: Arc<Mutex<ExecutionState>>,
}
impl Registration {
    #[cfg(test)]
    pub(super) fn new(session_id: SessionId, call_id: ToolCallId) -> Self {
        Self::for_execution(session_id, crate::test_support::tool_identity(&call_id))
    }
    pub(super) fn for_execution(
        session_id: SessionId,
        identity: crate::contract::ToolCallIdentity,
    ) -> Self {
        #[cfg(test)]
        let call_id = identity.call_id.clone();
        use super::super::background::{Lifetime, Registration as Work, WorkKind};
        let work = Work::new(session_id, Lifetime::Call(identity), WorkKind::Tool);
        let state = Arc::new(Mutex::new(ExecutionState::default()));
        let cancellation = state.clone();
        work.on_stop(move || cancel(&cancellation));
        #[cfg(test)]
        lock(process_observations()).insert((session_id, call_id.clone()), Arc::downgrade(&state));
        Self { work, state }
    }
    pub(super) fn is_cancelled(&self) -> bool {
        self.work.is_cancelled()
    }
    pub(super) fn finish(&self) -> bool {
        self.work.finish()
    }
    pub(super) fn attach_process(&self, pid: u32) -> ProcessGuard<'_> {
        let process = KillHandle::new(pid);
        let mut state = lock(&self.state);
        if state.cancelled {
            process.kill();
        } else {
            state.process = Some(process);
        }
        ProcessGuard { state: &self.state }
    }
}

pub(super) struct ProcessGuard<'a> {
    state: &'a Mutex<ExecutionState>,
}
impl ProcessGuard<'_> {
    pub(super) fn kill(&self) {
        cancel(self.state);
    }
}
impl Drop for ProcessGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            cancel(self.state);
        }
        lock(self.state).process = None;
    }
}
fn cancel(state: &Mutex<ExecutionState>) {
    let mut state = lock(state);
    state.cancelled = true;
    if let Some(process) = state.process.take() {
        process.kill();
    }
}

#[cfg(test)]
pub(crate) fn cancel_call(session: SessionId, call: &ToolCallId) -> bool {
    super::super::background::cancel_call(session, &crate::test_support::tool_identity(call))
}
#[cfg(test)]
fn cancel_session(session: SessionId) {
    super::super::background::close_session(session);
}

// Observation only: production cancellation and ownership live in background.
#[cfg(test)]
type Observations = Mutex<HashMap<(SessionId, ToolCallId), std::sync::Weak<Mutex<ExecutionState>>>>;
#[cfg(test)]
fn process_observations() -> &'static Observations {
    static STATES: OnceLock<Observations> = OnceLock::new();
    STATES.get_or_init(Mutex::default)
}
#[cfg(test)]
impl Drop for Registration {
    fn drop(&mut self) {
        lock(process_observations()).retain(|_, state| !state.ptr_eq(&Arc::downgrade(&self.state)));
    }
}
#[cfg(test)]
pub(super) fn is_running(session: SessionId, call: &ToolCallId) -> bool {
    lock(process_observations())
        .get(&(session, call.clone()))
        .and_then(std::sync::Weak::upgrade)
        .is_some_and(|state| lock(&state).process.is_some())
}
#[cfg(test)]
pub(super) fn is_session_queued(session: SessionId) -> bool {
    lock(session_queues()).contains_key(&session)
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
// Panic safety: `advance` is the only place a drained entry is removed, so a
// job that panics without it running would leave the session's queue
// permanently active -- every later call for that session would
// enqueue and never dispatch. `run_job` guarantees `advance` runs exactly
// once per job via an RAII guard (`AdvanceGuard`, below) constructed before
// `job()` and dropped after, regardless of whether `job()` returns normally
// or unwinds.

type Job = Box<dyn FnOnce() + Send>;

// An entry means a job is active; its deque holds only the waiting jobs.
type SessionQueues = Mutex<HashMap<SessionId, VecDeque<Job>>>;

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
    match queues.entry(session_id) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().push_back(job);
            return;
        }
        Entry::Vacant(entry) => {
            entry.insert(VecDeque::new());
        }
    }
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
    let next = queues.get_mut(&session_id).and_then(VecDeque::pop_front);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retiring_an_old_registration_keeps_its_replacement() {
        let session = SessionId::new();
        let call_id = ToolCallId("replacement".into());
        let old = Registration::new(session, call_id.clone());
        let new = Registration::new(session, call_id.clone());
        assert!(old.is_cancelled());
        drop(old);
        assert!(cancel_call(session, &call_id));
        assert!(new.is_cancelled());
    }

    #[test]
    fn cancelling_one_session_preserves_another_with_the_same_call_id() {
        let call_id = ToolCallId("shared".into());
        let first_session = SessionId::new();
        let first = Registration::new(first_session, call_id.clone());
        let second = Registration::new(SessionId::new(), call_id.clone());
        assert!(cancel_call(first_session, &call_id));
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
    }

    #[test]
    fn session_cleanup_cancels_every_job_without_stopping_other_sessions() {
        let session = SessionId::new();
        let first = Registration::new(session, ToolCallId("first".into()));
        let second = Registration::new(session, ToolCallId("second".into()));
        let other = Registration::new(SessionId::new(), ToolCallId("first".into()));
        cancel_session(session);
        assert!(first.is_cancelled());
        assert!(second.is_cancelled());
        assert!(!other.is_cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn a_child_attached_after_cancellation_is_killed() {
        use std::os::unix::process::CommandExt;
        use std::time::{Duration, Instant};
        let session = SessionId::new();
        let call_id = ToolCallId("late-child".into());
        let registration = Registration::new(session, call_id.clone());
        assert!(cancel_call(session, &call_id));
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30"])
            .process_group(0)
            .spawn()
            .unwrap();
        let _process = registration.attach_process(child.id());
        let deadline = Instant::now() + Duration::from_secs(5);
        let killed = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break !status.success();
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !killed {
            super::super::process::kill_process_tree(child.id());
        }
        child.wait().unwrap();
        assert!(killed, "the retired registration must stop a late process");
    }
}
