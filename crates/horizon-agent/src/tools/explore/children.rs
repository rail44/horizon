//! Every background `task` child this process has launched, keyed by the
//! child's own session id, plus the per-requester queue of completions that
//! have not yet been delivered as a notification.
//!
//! One process-wide `Mutex` covers both maps because every interesting
//! operation touches both: a completion moves a child from running to
//! finished *and* enqueues its notification, and a requester going away
//! drops its children *and* its queue. The registry is deliberately global
//! rather than hung off `ToolSessionState`: the two threads that read it --
//! the session loop that executes the tool and the rig turn loop that
//! drains notifications -- share no scope, and `ToolSessionState` is
//! `Rc`-based and thread-confined.
//!
//! Finished children are kept, not dropped: `task_output` must be able to
//! re-read a full report for as long as the requester session lives
//! (`docs/agent-async-task-design.md` decision 3). They are released
//! wholesale by [`take_all_for_requester`] when that session ends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::Sender;
use serde_json::Value;

use crate::contract::SessionId;

use super::ExplorationHost;

/// How many `task` children one requester may have in flight at once
/// (`docs/agent-async-task-design.md` decision 5). A fourth launch fails
/// fast rather than queueing: the model needs to learn immediately that it
/// is over budget, and a silent queue would make `task` look slow instead
/// of full.
pub(crate) const MAX_CONCURRENT_TASKS: usize = 3;

/// A child that has finished and whose result has not yet been delivered to
/// its requester.
#[derive(Clone, Debug)]
pub(super) struct Completion {
    pub(super) session_id: SessionId,
    pub(super) description: String,
    /// The child's full result value -- the same shape `task_output`
    /// returns (`report`/`capped`/`is_error`/`message`).
    pub(super) output: Value,
}

/// What a `task_output` lookup found.
pub(super) enum Lookup {
    /// No child with this id was ever launched by the asking session --
    /// either an unknown id or another session's child (the ownership
    /// check, which deliberately reports the same way for both: one
    /// session must not be able to probe another's ids).
    Unknown,
    Running {
        description: String,
    },
    Finished {
        description: String,
        output: Value,
    },
}

struct Child {
    requester: SessionId,
    description: String,
    /// Taken exactly once, by whichever of the waiter thread's own fold or
    /// [`take_all_for_requester`] gets there first -- that take-once
    /// semantics is the whole "terminate exactly once" gate.
    host: Option<Arc<dyn ExplorationHost>>,
    /// Stops the waiter thread's fold. Only used on requester teardown;
    /// children deliberately survive `cancel-turn` (decision 4).
    cancel: Sender<()>,
    /// `None` while the child is still running.
    outcome: Option<Value>,
}

#[derive(Default)]
struct Registry {
    children: HashMap<SessionId, Child>,
    pending: HashMap<SessionId, Vec<Completion>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `(session_id, description)` for every child of `requester` still
/// running -- the cap check's input, and the list named in the
/// over-cap error result so the model can decide what to wait for.
pub(super) fn running_for(requester: SessionId) -> Vec<(SessionId, String)> {
    let registry = lock();
    let mut running = registry
        .children
        .iter()
        .filter(|(_, child)| child.requester == requester && child.outcome.is_none())
        .map(|(id, child)| (*id, child.description.clone()))
        .collect::<Vec<_>>();
    // HashMap iteration order is arbitrary; a stable order keeps the error
    // message (and the test asserting it) deterministic.
    running.sort_by_key(|(id, _)| id.as_uuid());
    running
}

pub(super) fn register(
    requester: SessionId,
    child: SessionId,
    description: String,
    host: Arc<dyn ExplorationHost>,
    cancel: Sender<()>,
) {
    lock().children.insert(
        child,
        Child {
            requester,
            description,
            host: Some(host),
            cancel,
            outcome: None,
        },
    );
}

/// Test-only: registers `child` as a launched-but-unfinished task of
/// `requester` with no host behind it, so a test can drive the completion
/// half of the seam without standing up a scripted `ExplorationHost`.
#[cfg(test)]
pub(super) fn register_hostless(requester: SessionId, child: SessionId, description: &str) {
    let (cancel, _rx) = crossbeam_channel::bounded(1);
    lock().children.insert(
        child,
        Child {
            requester,
            description: description.to_string(),
            host: None,
            cancel,
            outcome: None,
        },
    );
}

/// Releases `child`'s host handle for termination, at most once.
pub(super) fn take_host(child: SessionId) -> Option<Arc<dyn ExplorationHost>> {
    lock()
        .children
        .get_mut(&child)
        .and_then(|child| child.host.take())
}

/// Records `child`'s final result and queues its notification for delivery.
/// Returns the requester to wake, or `None` if that session already went
/// away (its whole registry footprint is gone, so there is nobody to
/// notify).
pub(super) fn complete(child: SessionId, output: Value) -> Option<SessionId> {
    let mut registry = lock();
    let entry = registry.children.get_mut(&child)?;
    if entry.outcome.is_some() {
        // A second terminal fold for the same child: keep the first.
        return None;
    }
    entry.outcome = Some(output.clone());
    let requester = entry.requester;
    let completion = Completion {
        session_id: child,
        description: entry.description.clone(),
        output,
    };
    registry
        .pending
        .entry(requester)
        .or_default()
        .push(completion);
    Some(requester)
}

/// Drains every completion queued for `requester`. The queue is the
/// coalescing point: whatever landed since the last drain leaves together
/// as one notification block (`docs/agent-async-task-design.md` decision
/// 2).
pub(super) fn take_pending(requester: SessionId) -> Vec<Completion> {
    lock().pending.remove(&requester).unwrap_or_default()
}

/// Test-only, non-destructive: how many completions are queued for
/// `requester` right now. Lets a test wait for several children to land
/// *before* draining, which is what "coalesced between two rounds" means.
#[cfg(test)]
pub(super) fn pending_count(requester: SessionId) -> usize {
    lock()
        .pending
        .get(&requester)
        .map(Vec::len)
        .unwrap_or_default()
}

pub(super) fn lookup(requester: SessionId, child: SessionId) -> Lookup {
    let registry = lock();
    match registry.children.get(&child) {
        Some(entry) if entry.requester == requester => match &entry.outcome {
            Some(output) => Lookup::Finished {
                description: entry.description.clone(),
                output: output.clone(),
            },
            None => Lookup::Running {
                description: entry.description.clone(),
            },
        },
        _ => Lookup::Unknown,
    }
}

/// One child released by [`take_all_for_requester`], with everything its
/// caller needs to shut it down outside this module's lock.
pub(super) struct ReleasedChild {
    pub(super) session_id: SessionId,
    /// `None` when something else already claimed the termination (the
    /// child's own waiter finished first).
    pub(super) host: Option<Arc<dyn ExplorationHost>>,
    /// Stops the waiter thread's fold.
    pub(super) cancel: Sender<()>,
}

/// Removes every trace of `requester` -- children (running or finished) and
/// its undelivered queue -- and hands back what the caller must act on.
/// Termination is deliberately left to the caller so no `ExplorationHost`
/// call ever runs while this module's lock is held.
pub(super) fn take_all_for_requester(requester: SessionId) -> Vec<ReleasedChild> {
    let mut registry = lock();
    registry.pending.remove(&requester);
    let ids = registry
        .children
        .iter()
        .filter(|(_, child)| child.requester == requester)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    ids.into_iter()
        .filter_map(|id| {
            let mut child = registry.children.remove(&id)?;
            Some(ReleasedChild {
                session_id: id,
                host: child.host.take(),
                cancel: child.cancel.clone(),
            })
        })
        .collect()
}
