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
//! (`docs/agent-async-task-design.md` decision 3), and a child whose
//! session is still open can be sent a follow-up
//! (`docs/agent-subagent-design.md` decision 5). They are released
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
    /// returns (`report`/`is_error`/`message`).
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
    /// The report of the child's latest finished turn; `None` before its
    /// first turn has finished.
    outcome: Option<TaskReport>,
    /// Turns asked of the child -- its first prompt and each follow-up --
    /// whose report has not come back yet. The child is running while this
    /// is not zero. A follow-up can be sent while the child is still
    /// working; its session queues it, and each turn reports on its own.
    unanswered: u32,
    /// Whether the child's session is still there to take a follow-up.
    open: bool,
}

/// Why a follow-up could not be sent.
pub(super) enum FollowUpRefusal {
    /// See [`Lookup::Unknown`].
    Unknown,
    /// The child's session has ended.
    Closed,
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
            unanswered: 1,
            open: true,
        },
    );
}

/// Counts one more turn asked of `requester`'s own open `child`, labelled
/// `description` from now on. Called before the follow-up is sent, so a
/// child quick enough to answer before this returns is still expected.
pub(super) fn follow_up(
    requester: SessionId,
    child: SessionId,
    description: String,
) -> Result<(), FollowUpRefusal> {
    let mut registry = lock();
    match registry.children.get_mut(&child) {
        Some(entry) if entry.requester == requester && entry.open => {
            entry.unanswered += 1;
            entry.description = description;
            Ok(())
        }
        Some(entry) if entry.requester == requester => Err(FollowUpRefusal::Closed),
        _ => Err(FollowUpRefusal::Unknown),
    }
}

/// Takes back a follow-up [`follow_up`] counted but that could not be sent.
pub(super) fn withdraw_follow_up(child: SessionId) {
    if let Some(entry) = lock().children.get_mut(&child) {
        entry.unanswered = entry.unanswered.saturating_sub(1);
    }
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
            unanswered: 1,
            open: true,
        },
    );
}

/// Records the result of one of `child`'s turns and queues its notification
/// for delivery; `open` is whether the child's session is still there to
/// take a follow-up. Returns the requester to wake, or `None` if that
/// session already went away (its whole registry footprint is gone, so
/// there is nobody to notify) or no turn was outstanding.
pub(super) fn complete(child: SessionId, output: TaskReport, open: bool) -> Option<SessionId> {
    let mut registry = lock();
    let entry = registry.children.get_mut(&child)?;
    if entry.unanswered == 0 {
        // A terminal fold nobody asked for: keep what was reported.
        return None;
    }
    entry.unanswered = if open { entry.unanswered - 1 } else { 0 };
    entry.open = open;
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

/// The label `child` currently goes by: a follow-up may have relabelled it.
pub(super) fn description(child: SessionId) -> Option<String> {
    lock()
        .children
        .get(&child)
        .map(|entry| entry.description.clone())
}

pub(super) fn lookup(requester: SessionId, child: SessionId) -> Lookup {
    let registry = lock();
    match registry.children.get(&child) {
        Some(entry) if entry.requester == requester => match &entry.outcome {
            Some(output) if entry.unanswered == 0 => Lookup::Finished {
                output: output.clone(),
            },
            _ => Lookup::Running {
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
    if let Some(entry) = registry.children.remove(&child)
        && let Some(pending) = registry.pending.get_mut(&entry.requester)
    {
        pending.retain(|completion| completion.session_id != child);
    }
}
