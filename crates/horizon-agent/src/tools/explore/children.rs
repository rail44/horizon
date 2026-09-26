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
//! wholesale by [`remove_requester`] when that session ends.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::contract::tool_output::TaskReport;

use crate::contract::SessionId;

/// A child that has finished and whose result has not yet been delivered to
/// its requester.
#[derive(Clone, Debug)]
pub(super) struct Completion {
    pub(super) session_id: SessionId,
    pub(super) description: String,
    /// The child's full result value -- the same shape `task_output`
    /// returns (`report`/`capped`/`is_error`/`message`).
    pub(super) output: TaskReport,
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
        output: TaskReport,
    },
}

struct Child {
    requester: SessionId,
    description: String,
    /// `None` while the child is still running.
    outcome: Option<TaskReport>,
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

pub(in crate::tools) fn register(requester: SessionId, child: SessionId, description: String) {
    lock().children.insert(
        child,
        Child {
            requester,
            description,
            outcome: None,
        },
    );
}

/// Test-only: registers `child` as a launched-but-unfinished task of
/// `requester` with no host behind it, so a test can drive the completion
/// half of the seam without standing up a scripted `ExplorationHost`.
#[cfg(test)]
pub(super) fn register_hostless(requester: SessionId, child: SessionId, description: &str) {
    lock().children.insert(
        child,
        Child {
            requester,
            description: description.to_string(),
            outcome: None,
        },
    );
}

/// Records `child`'s final result and queues its notification for delivery.
/// Returns the requester to wake, or `None` if that session already went
/// away (its whole registry footprint is gone, so there is nobody to
/// notify).
pub(super) fn complete(child: SessionId, output: TaskReport) -> Option<SessionId> {
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

/// Records `child`'s final result without queueing a notification — what a
/// Mixture-of-Agents proposer uses. A queued notification would be injected
/// into the aggregator's `rig_history`; a proposal reaches it as the pass's
/// own provider-view block instead. The registration stays so `task_output`
/// can still re-read the proposer's full report.
pub(in crate::tools) fn complete_without_notification(child: SessionId, output: TaskReport) {
    let mut registry = lock();
    let Some(entry) = registry.children.get_mut(&child) else {
        return;
    };
    if entry.outcome.is_none() {
        entry.outcome = Some(output);
    }
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
                output: output.clone(),
            },
            None => Lookup::Running {
                description: entry.description.clone(),
            },
        },
        _ => Lookup::Unknown,
    }
}

/// Forget reports and notifications after the session's work has been stopped.
pub(super) fn remove_requester(requester: SessionId) {
    let mut registry = lock();
    registry.pending.remove(&requester);
    registry
        .children
        .retain(|_, child| child.requester != requester);
}

pub(super) fn remove_child(child: SessionId) {
    let mut registry = lock();
    if let Some(entry) = registry.children.remove(&child) {
        if let Some(pending) = registry.pending.get_mut(&entry.requester) {
            pending.retain(|completion| completion.session_id != child);
        }
    }
}
