//! `task` (`docs/agent-explore-design.md`, `docs/agent-async-task-
//! design.md`): delegate an open-ended investigation to a parallel,
//! read-only session sharing the requester's workspace, and fold only its
//! final report back into the requester's history.
//!
//! **Why this exists.** A session's history is monotonic -- every tool
//! result is retransmitted with every later provider request -- and
//! exploration output dominates it. The fragments a grep/read sweep
//! produces do not need to live in the requesting session's history at all;
//! only the conclusion does. So the sweep runs in a session of its own,
//! whose history is discarded with it.
//!
//! **Asynchronous since 2026-07-28** (`docs/agent-async-task-design.md`).
//! [`start`] returns immediately with `{session_id,
//! description, status: "started"}`; there is no blocking path left in this
//! tool. What used to be a waiter that stalled the requester's turn is now
//! a *completion subscription*: the waiter thread folds the child's event
//! stream, and when it reaches a terminal state the result is queued for
//! the requester and its session loop is woken (`notify::wake`). Delivery
//! is push, in two shapes:
//!
//! - the requester is mid-turn: the queue is drained before its next
//!   provider round and injected as one coalesced notification message
//!   (`providers::rig::session`'s injection);
//! - the requester's turn already ended: the wake starts a new turn with
//!   that notification as its synthetic input.
//!
//! Either way `Event::TurnEnded` stays the only turn boundary. A requester
//! sitting on an approval is mid-turn by construction (its batch is still
//! outstanding), so delivery waits for the approval to resolve without any
//! special case here.
//!
//! **Live progress.** Between launch and completion the watcher thread also
//! mirrors what the child is doing — which tool it last asked for, reasoning
//! vs tool-running — as ephemeral [`TaskProgress`] events forwarded through
//! [`ExplorationHost::forward_progress`] onto the requester's attachment
//! channel. These never touch conversation history or the event log; they
//! only feed the client's live task rows, and a row is retired when the
//! child reaches any terminal state (the durable record is the completion
//! notification, not the progress event).
//!
//! **Children are session-scoped, not turn-scoped** (decision 4): they
//! survive `cancel-turn` -- interrupting the requester must not vaporize
//! in-flight investigation -- and are terminated when the requesting
//! session itself goes away ([`cancel_session`], reached from
//! `unregister_session_runtime`). Host-restart cleanup is unchanged and
//! still keys on the explore role id alone (`roles::is_exploration`).
//!
//! **The seam.** This crate cannot spawn a session: hosting one is
//! `horizon-agent-host`'s job (`docs/agent-runtime-split-design.md`). So
//! [`ExplorationHost`] is a host-provided capability handle, installed on
//! `ToolSessionState` at session construction exactly like the recall
//! store, the network proxy, and the judge already are
//! (`ToolSessionBuilder::with_exploration_host`). `None` -- every test
//! construction, and any future host that can't spawn peers -- degrades to
//! an actionable error result, never a silent no-op. Its host-side
//! implementation is written against a named "subscribe to another
//! session's stop/completion events" abstraction
//! (`horizon-agent-host`'s `session::subscription`), so approval forwarding
//! for future write-capable children is one more event kind on the same
//! seam rather than a new one.
//!
//! **A child has no budget** (`docs/agent-subagent-design.md` decision 2):
//! what stops it is what stops any session. A child that stops before
//! finishing -- a guard, a provider failure, a lost session -- still
//! returns whatever report it has, with `message` saying why it stopped
//! ([`Outcome::into_output`]); the requester's notification calls the
//! report partial and the requester's pane gets a line for it
//! (decision 6).

pub(super) mod children;
mod notify;
pub(crate) mod worker;

use std::panic::AssertUnwindSafe;

use crossbeam_channel::Receiver;
#[cfg(test)]
use serde_json::Value;

use super::execution::ToolOutput;
use crate::contract::{
    Event, MessageRole, SessionId, SessionState, TaskProgress, TaskProgressState, ToolCallRequest,
    TurnEndReason,
};
use crate::tools::state::ToolSessionState;

pub(crate) use notify::{register_wake, unregister_wake};

/// The model-visible tool id. Renamed from `agent_explore` on 2026-07-27:
/// the plain `task` name measurably improved delegation adoption for one of
/// the two production models and was neutral for the other
/// (`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`,
/// cell C3).
///
/// Deliberately *not* the same string as `roles::EXPLORE_ROLE_ID`, which
/// stays `"explore"`. That one is a persistence and cleanup identity -- it
/// is written into the event log, decides which sessions
/// `horizon-agent-host` refuses to resume at startup, and filters the
/// client-visible session list -- so renaming it would touch resume paths
/// and already-persisted records while buying nothing the model can see.
pub(crate) const TOOL_ID: &str = "task";

/// The companion fetch tool's model-visible id
/// (`docs/agent-async-task-design.md` decision 3, the mainstream
/// `TaskOutput` shape): re-read one finished child's full
/// report, or learn that it is still running. Advertised only alongside
/// [`TOOL_ID`] -- see `providers::rig::completion::rig_tool_definitions`.
pub(crate) const OUTPUT_TOOL_ID: &str = "task_output";

/// A running exploration session, as handed back by the host.
pub struct StartedExploration {
    /// The spawned session's own id. Returned to the requester for cost
    /// attribution -- it is the join key between the requester's
    /// `ToolCallRequested`/`ToolCallFinished` records and the child's own
    /// rows in the event log and DuckDB projection (decision 3) -- and it
    /// is the handle `task_output` takes.
    pub session_id: SessionId,
    /// Every event the child session emits, in order, from before it has
    /// emitted any (the host installs the subscription *before* spawning
    /// the session, so nothing is missed).
    pub events: Receiver<Event>,
    /// The branch a write child works on, for its requester to integrate;
    /// `None` for a read child.
    pub branch: Option<String>,
}

/// What a `task` launch asks of the host.
pub struct TaskStart {
    /// The child's first user message -- its whole history.
    pub prompt: String,
    /// Read or write (`docs/agent-subagent-design.md` decision 1).
    pub access: crate::tools::input::TaskAccess,
    /// The requester's filesystem grants as they are at launch
    /// (`docs/agent-subagent-design.md` decision 12). A read child keeps
    /// only those that read.
    pub filesystem_grants: Vec<horizon_sandbox::FilesystemGrant>,
    /// The domains the requester may reach, as they are at launch.
    pub domains: Vec<String>,
}

/// The host capability `task` is built on: spawn a peer session,
/// subscribe to its events, terminate it. Implemented by `horizon-agent-host`
/// (`session::AgentHostExplorationHost`) and installed on the requester's
/// `ToolSessionState`; the requester's own workspace root, provider, and
/// session id are baked into the implementation at construction, so this
/// trait stays as narrow as the tool actually needs.
///
/// "Peer, not child" (`docs/agent-explore-design.md` decision 2): the
/// implementation must spawn the task session against the *same* workspace
/// root as the requester -- an isolated requester's worktree included --
/// with no isolation of its own and no derivation-tree edge. The
/// parent/child vocabulary this module uses for *lifetime* ("children are
/// session-scoped") is deliberately not a claim about code genealogy.
pub trait ExplorationHost: Send + Sync {
    /// Confirms termination after `terminate`. Hosts with asynchronous session
    /// teardown override this; the default covers synchronous in-process hosts.
    fn wait_stopped(&self, _session_id: SessionId, _timeout: std::time::Duration) -> bool {
        true
    }

    /// Spawns a task session on the requester's own provider and sends the
    /// prompt as its first user message -- the session's entire history;
    /// its only other seeding is the copy of the requester's grants. A read
    /// child shares the requester's root; a write child gets a worktree made
    /// for it from the requester's `HEAD`. `Err` carries a message suitable
    /// for the model to read as the tool's error result.
    fn start(&self, start: TaskStart) -> Result<StartedExploration, String>;

    /// Sends `prompt` to a session started by [`Self::start`] as its next
    /// user message: a follow-up (`docs/agent-subagent-design.md` decision
    /// 5). A session still working on an earlier turn takes it after that
    /// turn. `Err` carries a message for the model, as with `start`.
    fn send(&self, session_id: SessionId, prompt: String) -> Result<(), String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start -- when
    /// the child's session ends on its own, or when the requesting session
    /// goes away; a child that only finished a turn stays open for a
    /// follow-up. A no-op for a session that has already ended on its own.
    fn terminate(&self, session_id: SessionId);

    /// Shows `request`, raised by `child`, to the human answering for the
    /// requesting session (`docs/agent-subagent-design.md` decision 4); the
    /// answer goes straight to the child. `false` when nobody can answer --
    /// the default, and a requester that is itself unattended -- and the
    /// child then fails as a task that asked for an approval it can never
    /// receive.
    fn forward_approval(
        &self,
        _child: SessionId,
        _request: crate::contract::ApprovalRequest,
    ) -> bool {
        false
    }

    /// Withdraws a request [`Self::forward_approval`] showed: `child`
    /// resolved it.
    fn settle_approval(&self, _child: SessionId, _occurrence: crate::contract::OccurrenceId) {}

    /// Forwards one live progress observation about `child` to whoever is
    /// watching the requesting session (its attached client). Called by the
    /// child's watcher thread as the child acts; the host implementation
    /// routes it onto the requester's attachment event channel and drops it
    /// when no client is attached — progress is ephemeral by design (see
    /// `contract::TaskProgress`). The default no-op covers hosts without a
    /// client-attachment path (in-process test hosts).
    fn forward_progress(&self, _child: SessionId, _progress: TaskProgress) {}
}

/// Launches a `task` child and returns immediately. Unlike every other
/// asynchronous tool in this crate (`bash`, `web`), the *tool call* itself
/// finishes here and now -- an [`ToolOutput`] carrying the
/// `{session_id, description, status: "started"}` receipt -- because the
/// work it started is no longer part of this call. The child's eventual
/// report arrives on its own schedule as a notification (see the module
/// doc), not as this call's result.
pub(crate) fn start(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
    input: &crate::tools::input::Task,
) -> ToolOutput {
    let Some(host) = tool_state.exploration_host() else {
        return synchronous(
            request,
            error_output(format!("`{TOOL_ID}` is not available in this session")),
        );
    };
    // A session that may not write does not get its writing done by a
    // child either: a write child's worktree would hold the work it may
    // not do itself.
    if input.session_id.is_none()
        && input.access == crate::tools::input::TaskAccess::Write
        && let Some(read_only) = tool_state.read_only()
    {
        return synchronous(
            request,
            error_output(format!(
                "a write task cannot be started from this session: {}",
                read_only.guidance()
            )),
        );
    }
    if let Some(child) = input.session_id {
        return follow_up(
            &*host,
            session_id,
            SessionId::from_uuid(child),
            request,
            input,
        );
    }

    let work = super::background::Registration::new(
        session_id,
        super::background::Lifetime::Session,
        super::background::WorkKind::Child,
    );
    if work.is_cancelled() {
        return synchronous(request, error_output("session is stopping"));
    }
    let started = match host.start(TaskStart {
        prompt: input.prompt.to_string(),
        access: input.access,
        filesystem_grants: tool_state.retained_filesystem_grants(),
        domains: tool_state.allowed_domains(),
    }) {
        Ok(started) => started,
        Err(message) => {
            return synchronous(
                request,
                error_output(format!("could not start a task session: {message}")),
            );
        }
    };

    let child_id = started.session_id;
    let description = input.description.trim().to_string();
    children::register(
        session_id,
        child_id,
        description.clone(),
        started.branch.clone(),
    );
    let child_work = worker::ChildWork::attach(work, host.clone(), child_id);

    let events = started.events;
    let waiter_description = description.clone();
    std::thread::spawn(move || {
        // Live-progress emit point: the watcher owns the only consumer of
        // the child's event stream, so it is the one place that knows what
        // the child is doing. `host` was cloned into the registry above (the
        // take-once termination gate), leaving this move for the watcher.
        let emit = {
            let host = host.clone();
            let description = waiter_description.clone();
            move |state: TaskProgressState| {
                host.forward_progress(
                    child_id,
                    TaskProgress {
                        task_session_id: child_id,
                        description: description.clone(),
                        state,
                    },
                );
            }
        };
        // One fold per turn: a child that only finished a turn stays open
        // for a follow-up, and the next fold starts at that follow-up's
        // user message. The child is stopped only when its session ends.
        loop {
            let outcome = watch_until_terminal(
                &events,
                child_work.cancelled(),
                &mut |state| {
                    if !child_work.is_cancelled() {
                        emit(state)
                    }
                },
                &mut |approval| match approval {
                    ChildApproval::Requested(request) => host.forward_approval(child_id, *request),
                    ChildApproval::Resolved(occurrence) => {
                        host.settle_approval(child_id, occurrence);
                        true
                    }
                },
            );
            let open = !outcome.terminal.ends_session();
            if !open && !child_work.finish() {
                return;
            }
            // A view-side callback must not prevent the durable completion.
            let _ =
                std::panic::catch_unwind(AssertUnwindSafe(|| emit(TaskProgressState::Finished)));
            let description =
                children::description(child_id).unwrap_or_else(|| waiter_description.clone());
            let output = outcome.into_output(child_id, &description);
            if let Some(requester) = children::complete(child_id, output, open) {
                notify::wake(requester);
            }
            if !open {
                return;
            }
        }
    });

    synchronous(
        request,
        Response::succeeded(TaskOutput::Started {
            session_id: child_id.as_uuid().to_string(),
            description,
        }),
    )
}

/// `task` with a `session_id`: send `input.prompt` to one of this
/// session's own open children as a follow-up
/// (`docs/agent-subagent-design.md` decision 5). The child keeps its
/// history; its next report arrives as a notification, like the first.
fn follow_up(
    host: &dyn ExplorationHost,
    session_id: SessionId,
    child: SessionId,
    request: &ToolCallRequest,
    input: &crate::tools::input::Task,
) -> ToolOutput {
    let description = input.description.trim().to_string();
    match children::follow_up(session_id, child, description.clone()) {
        Ok(()) => {}
        Err(children::FollowUpRefusal::Unknown) => {
            return synchronous(
                request,
                error_output(format!(
                    "no task with session_id `{}` was launched from this session",
                    child.as_uuid()
                )),
            );
        }
        Err(children::FollowUpRefusal::Closed) => {
            return synchronous(
                request,
                error_output(format!(
                    "the task session `{}` has ended and cannot take a follow-up; launch a new \
                     task instead",
                    child.as_uuid()
                )),
            );
        }
    }
    if let Err(message) = host.send(child, input.prompt.to_string()) {
        children::withdraw_follow_up(child);
        return synchronous(
            request,
            error_output(format!("could not send the follow-up: {message}")),
        );
    }
    synchronous(
        request,
        Response::succeeded(TaskOutput::Started {
            session_id: child.as_uuid().to_string(),
            description,
        }),
    )
}

/// Tells `requester` that its write child `child` was ended by a `horizon-agent-acp`
/// restart (`docs/agent-subagent-design.md` decision 11): queued as that
/// child's last report, naming the branch its work was kept on, and
/// delivered like any other -- at once if the requester's loop is running,
/// else as soon as it starts.
pub fn deliver_restart_notice(
    requester: SessionId,
    child: SessionId,
    description: String,
    branch: Option<String>,
) {
    let message = match &branch {
        Some(_) => "the task session was stopped by a restart of the agent runtime; its work \
                    was kept on its branch"
            .to_string(),
        None => "the task session was stopped by a restart of the agent runtime before it \
                 committed or changed anything"
            .to_string(),
    };
    children::register(requester, child, description.clone(), branch);
    let report = TaskReport {
        session_id: child.as_uuid().to_string(),
        description,
        report: None,
        branch: None,
        message: Some(message),
    };
    if let Some(requester) = children::complete(child, report, false) {
        notify::wake(requester);
    }
}

/// Test-only: puts an already-finished child in the registry, so a test
/// elsewhere in this crate can exercise `task_output`'s re-fetch without
/// standing up a scripted `ExplorationHost` and driving a whole child
/// lifecycle. Used by `providers::rig::clearing`'s tests to pin the
/// interaction the compaction design depends on: clearing an old `task`
/// report out of the *provider view* must leave the report itself
/// re-fetchable, because finished children are retained for the
/// requester's lifetime and clearing touches nothing but one request's
/// message list.
#[cfg(test)]
pub(crate) fn register_finished_child_for_test(
    requester: SessionId,
    child: SessionId,
    description: &str,
    output: Value,
) {
    children::register_hostless(requester, child, description);
    children::complete(child, test_report(child, description, output), true);
}

/// `task_output`: the full report of one finished child owned by this
/// session (decision 3). Ownership is checked, and an id belonging to
/// another session reports exactly like an unknown one -- a session must
/// not be able to probe another's task ids.
pub(crate) fn output(
    session_id: SessionId,
    request: &ToolCallRequest,
    input: &crate::tools::input::TaskOutput,
) -> ToolOutput {
    let target = SessionId::from_uuid(input.session_id);

    let output = match children::lookup(session_id, target) {
        children::Lookup::Unknown => error_output(format!(
            "no task with session_id `{}` was launched from this session",
            target.as_uuid()
        )),
        children::Lookup::Running { description } => Response::succeeded(TaskOutput::Running {
            session_id: target.as_uuid().to_string(), description,
            message: "this task has not finished yet. If nothing else is ready to do, end your turn instead of polling: the completion notification starts a new turn on its own, and calling task_output again before then only costs a round.".into(),
        }),
        children::Lookup::Finished { output } => {
            let failed = output.failed();
            let body = TaskOutput::Finished { report: output };
            if failed { Response::failed(body) } else { Response::succeeded(body) }
        }
    };
    synchronous(request, output)
}

/// Terminates every child this session launched and drops its undelivered
/// notifications -- called from `unregister_session_runtime` when the
/// requesting session itself goes away. This is the *only* thing that kills
/// a child: a cancelled turn deliberately leaves them running (decision 4).
pub(crate) fn cancel_session(session_id: SessionId) {
    super::background::close_session(session_id);
    children::remove_requester(session_id);
    notify::unregister_wake(session_id);
}

/// One drain of a requester's finished-task queue.
pub(crate) struct TaskNotification {
    /// The single notification message the next provider round carries.
    pub(crate) text: String,
    /// One line per child that did not finish, for the requester's pane
    /// (`notify::unfinished_lines`).
    pub(crate) failures: Vec<String>,
}

/// Drains `session_id`'s finished-task queue into the single notification
/// message its next provider round should carry, or `None` when nothing is
/// waiting. Called by the rig session loop -- before each round, and again
/// when a wake signal arrives with the turn already ended.
pub(crate) fn take_notification(session_id: SessionId) -> Option<TaskNotification> {
    let completions = children::take_pending(session_id);
    if completions.is_empty() {
        return None;
    }
    Some(TaskNotification {
        text: notify::notification_text(&completions),
        failures: notify::unfinished_lines(&completions),
    })
}

/// The `contract::Event` a delivered notification is recorded as. The role
/// is deliberately [`MessageRole::TaskNotification`] rather than `User`:
/// the provider is sent plain user-role text (see `notify`'s module doc for
/// the template-safety argument), but the event log must not record a
/// system notification as words a human typed.
pub(crate) fn notification_event(text: String) -> Event {
    Event::MessageCommitted(crate::contract::Message {
        role: MessageRole::TaskNotification,
        text,
    })
}

/// Test-only: delivers a finished `task` child to `requester` exactly the
/// way the waiter thread does -- queue it, then wake the session loop --
/// without needing a scripted [`ExplorationHost`] behind it. This is how
/// `providers::rig`'s session-loop tests exercise notification injection
/// and the auto-turn wake against the real loop.
#[cfg(test)]
pub(crate) fn deliver_test_completion(
    requester: SessionId,
    child: SessionId,
    description: &str,
    output: Value,
) {
    children::register_hostless(requester, child, description);
    if let Some(requester) =
        children::complete(child, test_report(child, description, output), true)
    {
        notify::wake(requester);
    }
}

/// A tool call that resolves right now -- which, since the 2026-07-28
/// asynchronous cutover, is *every* `task`/`task_output` call: launching is
/// no longer something the call waits on.
fn synchronous(_request: &ToolCallRequest, output: Response) -> ToolOutput {
    output.into()
}

use super::output::{Response, TaskOutput, TaskReport, error as error_output};

/// Reasoning-close tags a serving layer can leak into an assistant message
/// with no opening tag anywhere -- the `</mm:think>` shape observed
/// 2026-07-28 (`docs/research/agent-harness-findings-97-2026-07-28.md`).
const ORPHAN_REASONING_CLOSE_TAGS: [&str; 3] = ["</mm:think>", "</thinking>", "</think>"];

/// Whether a child's report is empty once stray reasoning-close artifacts
/// are discounted.
///
/// A child killed by a provider error mid-thought can leave a "partial
/// report" whose entire body is one orphan close tag; delivering that as
/// content reads like an answer and is worse than saying nothing came back.
/// This is an *emptiness test only* -- the stored report keeps whatever the
/// child actually emitted, because defensively rewriting bodies Horizon does
/// not understand is a separate, open decision.
pub(super) fn report_body_is_empty(report: &str) -> bool {
    let mut rest = report.trim();
    loop {
        let Some(stripped) = ORPHAN_REASONING_CLOSE_TAGS
            .iter()
            .find_map(|tag| rest.strip_prefix(tag))
        else {
            return rest.is_empty();
        };
        rest = stripped.trim();
    }
}

/// How a child's turn -- or its whole event stream -- ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Terminal {
    /// The child's turn ended normally -- the report is final.
    Completed,
    /// The turn ended some other way (failed, cancelled, or halted by a
    /// guard). Whatever report exists is still returned. `None` when
    /// the stream reached `WaitingForUser` with no report and no preceding
    /// `TurnEnded` at all -- the turn ended some way this watcher never saw
    /// named.
    TurnEnded(Option<TurnEndReason>),
    /// The child asked for an approval nobody can answer: its requester
    /// could not show it to a human (`ExplorationHost::forward_approval`).
    /// The task fails at once rather than waiting forever.
    Approval,
    /// The session terminated without ending its turn.
    Terminated,
    /// The event stream ended without a terminal event -- the session's
    /// thread is gone.
    Disconnected,
    /// The requesting session went away.
    Cancelled,
    /// The fold itself unwound.
    Panicked(String),
}

impl Terminal {
    /// Whether the child's session is gone (or must go) with this ending,
    /// rather than only its turn: a child that only ended a turn stays
    /// open for a follow-up. An approval the child can never receive ends
    /// it too, as before.
    fn ends_session(&self) -> bool {
        match self {
            Terminal::Completed | Terminal::TurnEnded(_) => false,
            Terminal::Approval
            | Terminal::Terminated
            | Terminal::Disconnected
            | Terminal::Cancelled
            | Terminal::Panicked(_) => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Outcome {
    pub(super) terminal: Terminal,
    /// The last assistant message committed after the child's own user
    /// message -- "the session's final assistant text". Messages committed
    /// before that (the provider's own initialization notice) and mid-turn
    /// narration superseded by a later message are deliberately not part of
    /// it.
    pub(super) report: Option<String>,
    /// The most recent `Event::Error` message, used only to explain a
    /// failure the terminal reason alone doesn't.
    pub(super) error: Option<String>,
}

impl Outcome {
    pub(super) fn into_output(self, session_id: SessionId, description: &str) -> TaskReport {
        let message = self.failure_message();
        TaskReport {
            session_id: session_id.as_uuid().to_string(),
            description: description.into(),
            report: self.report,
            branch: None,
            message,
        }
    }

    /// A report with an actual body behind it -- a stored report that is
    /// nothing but a stray reasoning-close artifact does not count as one,
    /// so a child that shipped only that is reported as the failure it was
    /// (see [`report_body_is_empty`]).
    pub(super) fn has_usable_report(&self) -> bool {
        self.report
            .as_deref()
            .is_some_and(|report| !report_body_is_empty(report))
    }

    fn failure_message(&self) -> Option<String> {
        let detail = |suffix: &str| match &self.error {
            Some(error) => format!("{suffix} ({error})"),
            None => suffix.to_string(),
        };
        match &self.terminal {
            Terminal::Completed if self.has_usable_report() => None,
            Terminal::Completed => Some(detail(
                "the task session ended its turn without producing a usable report",
            )),
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByTokenBudget)) => Some(detail(
                "the task session spent its token budget and was stopped",
            )),
            Terminal::TurnEnded(None) => {
                Some(detail("the task session's turn ended before it finished"))
            }
            Terminal::TurnEnded(Some(reason)) => Some(detail(&format!(
                "the task session's turn ended as {reason:?} before it finished"
            ))),
            Terminal::Approval => Some(
                "the task session asked for an approval it can never receive; nothing was run"
                    .to_string(),
            ),
            Terminal::Terminated => Some(detail("the task session terminated before finishing")),
            Terminal::Disconnected => Some(detail("the task session stopped responding")),
            // A cancelled child's requester is gone, so `children::complete`
            // finds nothing to queue this against; the arm exists only for
            // exhaustiveness.
            Terminal::Cancelled => Some("the task was cancelled".to_string()),
            Terminal::Panicked(message) => Some(format!("the task waiter panicked: {message}")),
        }
    }
}

/// An approval event in a child's stream, for the watcher to pass on.
pub(super) enum ChildApproval {
    Requested(Box<crate::contract::ApprovalRequest>),
    Resolved(crate::contract::OccurrenceId),
}

/// Turns watcher failures into a terminal outcome so task callers can still
/// retire their child and publish its result. Termination
/// and notification policy remain with the caller. `on_approval` passes a
/// child's approval on; `false` for a request means nobody can answer it.
pub(super) fn watch_until_terminal(
    events: &Receiver<Event>,
    cancel: &Receiver<()>,
    on_state: &mut dyn FnMut(TaskProgressState),
    on_approval: &mut dyn FnMut(ChildApproval) -> bool,
) -> Outcome {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        fold_until_terminal(events, cancel, on_state, on_approval)
    }))
    .unwrap_or_else(|payload| Outcome {
        terminal: Terminal::Panicked(crate::runtime_panic::panic_message(&*payload)),
        report: None,
        error: None,
    })
}

/// Folds the child's event stream until it reaches a terminal state or the
/// requesting session goes away. Pure over its receivers and the
/// `on_state` callback, so the whole decision table above is
/// unit-testable by feeding a scripted event sequence: every time what a
/// live progress row would say changes (running, or waiting for an
/// approval), the observation is handed to `on_state`. Terminal emission is
/// the caller's — the fold only reports the states before it.
fn fold_until_terminal(
    events: &Receiver<Event>,
    cancel: &Receiver<()>,
    on_state: &mut dyn FnMut(TaskProgressState),
    on_approval: &mut dyn FnMut(ChildApproval) -> bool,
) -> Outcome {
    let mut report = None;
    let mut error = None;
    // The child's own user message is the boundary: everything committed
    // before it belongs to session startup, not to the answer.
    let mut turn_started = false;
    // Live-progress tracking: what the child was last observed doing, and
    // the last observation actually emitted (dedup — history-shaped events
    // like `MessageCommitted` must not re-emit an unchanged row).
    let mut state = TaskProgressState::Running;
    let mut emitted: Option<TaskProgressState> = None;

    let terminal = loop {
        crossbeam_channel::select_biased! {
            recv(cancel) -> _ => break Terminal::Cancelled,
            recv(events) -> received => {
                let Ok(event) = received else {
                    break Terminal::Disconnected;
                };
                match event {
                    Event::MessageCommitted(message) => match message.role {
                        MessageRole::User => {
                            turn_started = true;
                            report = None;
                        }
                        _ if turn_started && !message.text.trim().is_empty() => {
                            report = Some(message.text);
                        }
                        _ => {}
                    },
                    // A child's approval is shown to the human through its
                    // requester and the child waits for the answer; only
                    // when nobody can answer does the task fail.
                    Event::ApprovalRequested(request) => {
                        state = TaskProgressState::WaitingForApproval;
                        if !on_approval(ChildApproval::Requested(Box::new(request))) {
                            break Terminal::Approval;
                        }
                    }
                    Event::ApprovalResolved(resolved) => {
                        state = TaskProgressState::Running;
                        on_approval(ChildApproval::Resolved(resolved.occurrence_id));
                    }
                    Event::TurnEnded(reason) if turn_started => {
                        break match reason {
                            TurnEndReason::Completed => Terminal::Completed,
                            other => Terminal::TurnEnded(Some(other)),
                        };
                    }
                    // Fallback end-of-turn signal for a provider that does
                    // not emit `TurnEnded` at all. The rig provider always
                    // emits it first (`apply_turn_outcome`), so this only
                    // ever fires for one that doesn't. One caveat kept on
                    // record: rig ALSO emits `WaitingForUser` mid-turn in
                    // two measured shapes (the startup/`Initialize` pair,
                    // and approval-gated async-tool boundaries — backlog
                    // 47). Both are unreachable in a read task child (the
                    // host never sends `Initialize` after the user message,
                    // an approval that would need a human is refused rather
                    // than raised, and task children are never resumed
                    // across a `horizon-agent-acp` restart; a follow-up is a new user
                    // message, which starts the next fold) — but `bash` is
                    // now allowed and can run in the background, and a
                    // write child will raise approvals, so this arm is a
                    // premature-completion hazard to revisit with them.
                    Event::StateChanged(SessionState::WaitingForUser) if turn_started => {
                        break match report {
                            Some(_) => Terminal::Completed,
                            None => Terminal::TurnEnded(None),
                        };
                    }
                    Event::StateChanged(SessionState::Terminated | SessionState::Suspended)
                    | Event::Exited(_) => {
                        break Terminal::Terminated;
                    }
                    Event::Error(failure) => error = Some(failure.message),
                    // Back in `Running`, the child is no longer waiting.
                    Event::StateChanged(SessionState::Running) => {
                        state = TaskProgressState::Running;
                    }
                    Event::StateChanged(SessionState::ToolRunning) => {}
                    // Guard-fail fallbacks: `TurnEnded` and
                    // `StateChanged(WaitingForUser)` above are guarded on
                    // `turn_started`; before the child's own user message that
                    // guard fails and they land here as no-ops (session
                    // startup, not an answer). The remaining `StateChanged(_)`
                    // states also absorb the non-terminal session states this
                    // watcher never acts on. Grouped so the match stays
                    // exhaustive without a wildcard -- adding a variant is a
                    // compile error here, forcing a decision (see
                    // docs/agent-event-readers.md).
                    Event::TurnEnded(_)
                    | Event::StateChanged(_)
                    // Streaming deltas don't change the outcome: the answer is
                    // captured from `MessageCommitted` text, not from deltas.
                    | Event::ReasoningDelta(_)
                    | Event::AssistantTextDelta(_)
                    // Tool lifecycle is not terminal here -- the watcher waits
                    // for the turn's final report/approval/exit, not individual
                    // tool calls.
                    | Event::ToolCallRequested(_)
                    | Event::ToolCallStarted(_)
                    | Event::ToolCallFinished(_)
                    // Provider request lifecycle markers are timing-only and
                    // carry no terminal signal.
                    | Event::ProviderRequestSent(_)
                    | Event::ProviderRequestFirstToken
                    | Event::ProviderRequestFinished
                    | Event::ProviderRequestUsage(_)
                    // Tier 1 clearing is a provider-view projection, not a
                    // terminal event for this child watcher.
                    | Event::HistoryCleared(_)
                    // Operator-intervention audit events: audit-only, not
                    // terminal.
                    | Event::ContinueTurnRequested(_)
                    | Event::ContinuationVerdict(_)
                    // Provider rate-limit pacing is not terminal.
                    | Event::ProviderRateLimited(_)
                    | Event::SessionInputSent { .. } | Event::EnvironmentReady { .. } | Event::EnvironmentActivated(_) | Event::EnvironmentActivationFailed(_) | Event::SessionResumed | Event::Rewound { .. } | Event::ModelSelected { .. } | Event::ReasoningSelected { .. } | Event::SessionTitled { .. } | Event::InputQueuePaused(_) | Event::InputStarted(_) | Event::InputAccepted(_) | Event::InputOutcome(_) | Event::DeliveryAcknowledged(_) | Event::ConversationRecorded(_) => {}
                }
                if emitted != Some(state) {
                    on_state(state);
                    emitted = Some(state);
                }
            },
        }
    };

    Outcome {
        terminal,
        report,
        error,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
fn test_report(child: SessionId, description: &str, output: Value) -> TaskReport {
    TaskReport {
        session_id: child.as_uuid().to_string(),
        description: description.into(),
        report: output
            .get("report")
            .and_then(Value::as_str)
            .map(str::to_owned),
        branch: output
            .get("branch")
            .and_then(Value::as_str)
            .map(str::to_owned),
        message: output
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}
