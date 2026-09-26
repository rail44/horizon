//! Own cancellation and completion separately: a stop request is not a join.
//! Register before launching work, and keep the registration through delivery.
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::contract::{SessionId, ToolCallIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Lifetime {
    Call(ToolCallIdentity),
    Pass(Uuid),
    Session,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkKind {
    Tool,
    Judgment,
    Child,
}

type Stop = Box<dyn FnOnce() + Send>;
enum Phase {
    Active(Option<Stop>),
    Finished,
    Cancelled,
}

struct Entry {
    lifetime: Lifetime,
    kind: WorkKind,
    token: CancellationToken,
    phase: Mutex<Phase>,
}

impl Entry {
    fn stop(&self, finished: bool) -> bool {
        let action = {
            let mut phase = lock(&self.phase);
            if !matches!(*phase, Phase::Active(_)) {
                return false;
            }
            let next = if finished {
                Phase::Finished
            } else {
                Phase::Cancelled
            };
            let Phase::Active(action) = std::mem::replace(&mut *phase, next) else {
                unreachable!()
            };
            if !finished {
                self.token.cancel();
            }
            action
        };
        if let Some(action) = action {
            action();
        }
        true
    }
}

#[derive(Default)]
struct GroupState {
    owned: bool,
    closed: bool,
    entries: HashMap<Uuid, Arc<Entry>>,
}

#[derive(Default)]
struct Group {
    state: Mutex<GroupState>,
    settled: Condvar,
}

fn groups() -> &'static Mutex<HashMap<SessionId, Arc<Group>>> {
    static GROUPS: OnceLock<Mutex<HashMap<SessionId, Arc<Group>>>> = OnceLock::new();
    GROUPS.get_or_init(Mutex::default)
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The runtime owns this handle; worker registrations may outlive it while
/// stopping. Closing it prevents registrations racing with session teardown.
pub(crate) struct SessionOwner {
    session: SessionId,
    group: Arc<Group>,
}
impl SessionOwner {
    pub(crate) fn new(session: SessionId) -> Self {
        let mut groups = lock(groups());
        let group = groups.entry(session).or_default().clone();
        lock(&group.state).owned = true;
        Self { session, group }
    }
}
impl Drop for SessionOwner {
    fn drop(&mut self) {
        close_group(&self.group);
        let mut groups = lock(groups());
        let mut state = lock(&self.group.state);
        state.owned = false;
        if state.entries.is_empty() {
            groups.remove(&self.session);
        }
    }
}

pub(crate) struct Registration {
    session: SessionId,
    id: Uuid,
    group: Arc<Group>,
    entry: Arc<Entry>,
}
impl Registration {
    pub(crate) fn new(session: SessionId, lifetime: Lifetime, kind: WorkKind) -> Self {
        let entry = Arc::new(Entry {
            lifetime,
            kind,
            token: CancellationToken::new(),
            phase: Mutex::new(Phase::Active(None)),
        });
        let id = Uuid::new_v4();
        let (group, replaced, closed) = {
            let mut groups = lock(groups());
            let group = groups.entry(session).or_default().clone();
            let mut state = lock(&group.state);
            let replaced = state
                .entries
                .values()
                .filter(|old| {
                    matches!((&entry.lifetime, &old.lifetime), (Lifetime::Call(new), Lifetime::Call(old)) if new.call_id == old.call_id)
                        && old.kind == kind
                })
                .cloned()
                .collect::<Vec<_>>();
            state.entries.insert(id, entry.clone());
            let closed = state.closed;
            drop(state);
            (group, replaced, closed)
        };
        stop_all(replaced);
        if closed {
            entry.stop(false);
        }
        Self {
            session,
            id,
            group,
            entry,
        }
    }

    /// Install the resource-specific stop action after resource acquisition.
    /// A stop that won the race executes it immediately instead of losing it.
    pub(crate) fn on_stop(&self, stop: impl FnOnce() + Send + 'static) {
        let mut phase = lock(&self.entry.phase);
        if let Phase::Active(action) = &mut *phase {
            assert!(action.is_none(), "one resource owner per registration");
            *action = Some(Box::new(stop));
        } else {
            drop(phase);
            stop();
        }
    }
    pub(crate) fn token(&self) -> CancellationToken {
        self.entry.token.clone()
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.entry.token.is_cancelled()
    }
    /// Wins at most once against cancellation. Hold the registration until
    /// the completion has been sent, so drain also waits for publication.
    pub(crate) fn finish(&self) -> bool {
        self.entry.stop(true)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // Release accounting even if a resource-specific destructor panics.
        struct Retire<'a>(&'a Registration);
        impl Drop for Retire<'_> {
            fn drop(&mut self) {
                let work = self.0;
                let mut groups = lock(groups());
                let mut state = lock(&work.group.state);
                state.entries.remove(&work.id);
                if state.entries.is_empty() && !state.owned {
                    groups.remove(&work.session);
                }
                work.group.settled.notify_all();
            }
        }
        let _retire = Retire(self);
        self.entry.stop(false);
    }
}

fn cancel_matching(session: SessionId, predicate: impl Fn(&Entry) -> bool) -> bool {
    let group = lock(groups()).get(&session).cloned();
    if let Some(group) = group {
        let entries = lock(&group.state)
            .entries
            .values()
            .filter(|entry| predicate(entry))
            .cloned()
            .collect::<Vec<_>>();
        let found = !entries.is_empty();
        stop_all(entries);
        found
    } else {
        false
    }
}
fn stop_all(entries: Vec<Arc<Entry>>) {
    // A faulty host must not prevent other resources from being stopped.
    for entry in entries {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.stop(false))).is_err() {
            eprintln!("background resource stop panicked");
        }
    }
}
pub(crate) fn cancel_call(session: SessionId, call: &ToolCallIdentity) -> bool {
    cancel_matching(session, |entry| {
        entry.lifetime == Lifetime::Call(call.clone())
    })
}
pub(crate) fn cancel_judgment(session: SessionId, call: &ToolCallIdentity) {
    cancel_matching(session, |entry| {
        entry.kind == WorkKind::Judgment && entry.lifetime == Lifetime::Call(call.clone())
    });
}
pub(crate) fn cancel_pass(session: SessionId, pass: Uuid) {
    cancel_matching(session, |entry| entry.lifetime == Lifetime::Pass(pass));
}
fn close_group(group: &Group) {
    let entries = {
        let mut state = lock(&group.state);
        state.closed = true;
        state.entries.values().cloned().collect()
    };
    stop_all(entries);
}

/// Bounded waiting never pretends a timed-out worker has stopped. The daemon
/// retains the worktree when this returns false.
pub fn drain_session_work(session: SessionId, timeout: Duration) -> bool {
    let Some(group) = lock(groups()).get(&session).cloned() else {
        return true;
    };
    close_group(&group);
    let deadline = Instant::now() + timeout;
    let mut state = lock(&group.state);
    while !state.entries.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        state = group
            .settled
            .wait_timeout(state, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    true
}
pub(crate) fn close_session(session: SessionId) {
    let group = lock(groups()).get(&session).cloned();
    if let Some(group) = group {
        close_group(&group);
    }
}

/// Only environment-sensitive tools block a cwd change. Session children
/// and approval requests do not become a new environment-switch policy.
pub fn session_tool_work_settled(session: SessionId) -> bool {
    let group = lock(groups()).get(&session).cloned();
    group.is_none_or(|group| {
        !lock(&group.state)
            .entries
            .values()
            .any(|entry| entry.kind == WorkKind::Tool)
    })
}

#[cfg(test)]
mod tests;
