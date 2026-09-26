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
//! `unregister_session_runtime`). Daemon-restart cleanup is unchanged and
//! still keys on the explore role id alone (`roles::is_exploration`).
//!
//! **The seam.** This crate cannot spawn a session: hosting one is
//! `horizon-agentd`'s job (`docs/agent-runtime-split-design.md`). So
//! [`ExplorationHost`] is a daemon-provided capability handle, installed on
//! `ToolSessionState` at session construction exactly like the recall
//! store, the network proxy, and the judge already are
//! (`ToolSessionBuilder::with_exploration_host`). `None` -- every test
//! construction, and any future host that can't spawn peers -- degrades to
//! an actionable error result, never a silent no-op. Its daemon-side
//! implementation is written against a named "subscribe to another
//! session's stop/completion events" abstraction
//! (`horizon-agentd`'s `session::subscription`), so approval forwarding
//! for future write-capable children is one more event kind on the same
//! seam rather than a new one.
//!
//! **Iteration-cap exhaustion is a partial success, not an error.** A role
//! that opts in (`RoleDefinition::summarize_on_cap`, set for
//! `EXPLORE_ROLE` -- `docs/research/agent-context-reduction-prior-
//! art-2026-07-26.md` §4's OpenCode/Hermes precedent) gets one forced,
//! tools-disabled completion before the rig turn loop halts on the cap
//! (`providers::rig::session::halt_turn_loop`), so the child reports
//! whatever it found instead of discarding the work behind a generic error.
//! [`Outcome::into_output`] surfaces that as an ordinary (`is_error:
//! false`) result carrying a `capped: true` marker, not a failure.

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
use crate::roles::RoleId;
use crate::tools::state::ToolSessionState;

pub(crate) use notify::{register_wake, unregister_wake};

/// The model-visible tool id. Renamed from `agent.explore` on 2026-07-27:
/// the plain `task` name measurably improved delegation adoption for one of
/// the two production models and was neutral for the other
/// (`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`,
/// cell C3).
///
/// Deliberately *not* the same string as `roles::EXPLORE_ROLE_ID`, which
/// stays `"explore"`. That one is a persistence and cleanup identity -- it
/// is written into the event log, decides which sessions
/// `horizon-agentd` refuses to resume at startup, and filters the
/// client-visible session list -- so renaming it would touch resume paths
/// and already-persisted records while buying nothing the model can see.
pub(crate) const TOOL_ID: &str = "task";

/// The companion fetch tool's model-visible id
/// (`docs/agent-async-task-design.md` decision 3, the mainstream
/// `TaskOutput`/`bash_output` shape): re-read one finished child's full
/// report, or learn that it is still running. Advertised only alongside
/// [`TOOL_ID`] -- see `providers::rig::completion::rig_tool_definitions`.
pub(crate) const OUTPUT_TOOL_ID: &str = "task_output";

/// A running exploration session, as handed back by the daemon.
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
}

/// What one `task`-shaped session is started with. A `task` call names only
/// a prompt and the requester's own provider answers it; a Mixture-of-Agents
/// proposer (`docs/agent-moa-design.md`) names the other two, because each
/// member runs on its own `{provider, model}`.
pub struct ExplorationRequest {
    pub prompt: String,
    /// The `[[providers]]` entry name to run this session on. `None` — every
    /// `task` call — means the requesting session's own provider.
    pub provider: Option<String>,
    /// The model id to pin, written out (never an alias). `None` means the
    /// entry's own default model.
    pub model: Option<String>,
    /// The role the spawned session runs with: `roles::EXPLORE_ROLE_ID` for
    /// a `task` child, `roles::MOA_PROPOSER_ROLE_ID` for a proposer. Both
    /// satisfy `roles::is_exploration`, so the host treats them identically
    /// everywhere else; they differ only in the prompt section the session
    /// is given.
    pub role: RoleId,
}

impl ExplorationRequest {
    /// A prompt answered by the requesting session's own provider, in a
    /// `task` child.
    pub fn for_prompt(prompt: String) -> Self {
        Self {
            prompt,
            provider: None,
            model: None,
            role: RoleId(crate::roles::EXPLORE_ROLE_ID.to_string()),
        }
    }

    /// One Mixture-of-Agents proposer, pinned to its member's entry and
    /// model (`docs/agent-moa-design.md`).
    pub fn for_proposer(prompt: String, provider: String, model: String) -> Self {
        Self {
            prompt,
            provider: Some(provider),
            model: Some(model),
            role: RoleId(crate::roles::MOA_PROPOSER_ROLE_ID.to_string()),
        }
    }
}

/// The daemon capability `task` is built on: spawn a peer session,
/// subscribe to its events, terminate it. Implemented by `horizon-agentd`
/// (`session::AgentdExplorationHost`) and installed on the requester's
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

    /// Spawns a read-only task session and sends the request's prompt as its
    /// first user message -- the session's entire history; there is no other
    /// seeding. `Err` carries a message suitable for the model to read as
    /// the tool's error result.
    fn start(&self, request: ExplorationRequest) -> Result<StartedExploration, String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start -- when
    /// the child's own turn ends, or when the requesting session goes away.
    /// A no-op for a session that has already ended on its own.
    fn terminate(&self, session_id: SessionId);

    /// Forwards one live progress observation about `child` to whoever is
    /// watching the requesting session (its attached client). Called by the
    /// child's watcher thread as the child acts; the daemon implementation
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

    let work = super::background::Registration::new(
        session_id,
        super::background::Lifetime::Session,
        super::background::WorkKind::Child,
    );
    if work.is_cancelled() {
        return synchronous(request, error_output("session is stopping"));
    }
    let started = match host.start(ExplorationRequest::for_prompt(input.prompt.to_string())) {
        Ok(started) => started,
        Err(message) => {
            return synchronous(
                request,
                error_output(format!("could not start a task session: {message}")),
            )
        }
    };

    let child_id = started.session_id;
    let description = input.description.trim().to_string();
    let started_at_epoch_ms = unix_epoch_ms();
    children::register(session_id, child_id, description.clone());
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
            move |state: TaskProgressState, activity: Option<String>| {
                host.forward_progress(
                    child_id,
                    TaskProgress {
                        task_session_id: child_id,
                        description: description.clone(),
                        state,
                        activity,
                        started_at_epoch_ms,
                    },
                );
            }
        };
        let outcome = watch_until_terminal(&events, child_work.cancelled(), &mut |activity| {
            if !child_work.is_cancelled() {
                emit(TaskProgressState::Running, activity)
            }
        });
        if !child_work.finish() {
            return;
        }
        // A view-side callback must not prevent the durable completion.
        let _ =
            std::panic::catch_unwind(AssertUnwindSafe(|| emit(TaskProgressState::Finished, None)));
        let output = outcome.into_output(child_id, &waiter_description);
        if let Some(requester) = children::complete(child_id, output) {
            notify::wake(requester);
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
    children::complete(child, test_report(child, description, output));
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
    /// One line per child that produced no usable report, for the
    /// requester's pane (`notify::failure_lines`).
    pub(crate) failures: Vec<String>,
}

/// Drains `session_id`'s finished-task queue into the single notification
/// message its next provider round should carry, or `None` when nothing is
/// waiting. Called by the rig session loop -- before each round, and again
/// when a wake signal arrives with the turn already ended.
pub(crate) fn take_notification(session_id: SessionId) -> Option<TaskNotification> {
    let completions = children::take_pending(session_id);
    (!completions.is_empty()).then(|| TaskNotification {
        text: notify::notification_text(&completions),
        failures: notify::failure_lines(&completions),
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
    if let Some(requester) = children::complete(child, test_report(child, description, output)) {
        notify::wake(requester);
    }
}

/// A tool call that resolves right now -- which, since the 2026-07-28
/// asynchronous cutover, is *every* `task`/`task_output` call: launching is
/// no longer something the call waits on.
fn synchronous(_request: &ToolCallRequest, output: Response) -> ToolOutput {
    output.into()
}

use super::output::{error as error_output, Response, TaskOutput, TaskReport};

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

/// How a child's event stream ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Terminal {
    /// The child's turn ended normally -- the report is final.
    Completed,
    /// The turn ended some other way (failed, cancelled, or halted by a
    /// guard). Whatever report exists is still returned. For
    /// `HaltedByIterationCap` specifically, a report here means the rig turn
    /// loop's forced wrap-up completion succeeded (`RoleDefinition::
    /// summarize_on_cap`) -- see [`Outcome::failure_message`]. `None` when
    /// the stream reached `WaitingForUser` with no report and no preceding
    /// `TurnEnded` at all -- the turn ended some way this watcher never saw
    /// named.
    TurnEnded(Option<TurnEndReason>),
    /// The child parked on an approval. A task session is marked unattended
    /// (`ToolSessionState::is_unattended`), so a call that would need a
    /// human resolves as a refused tool result and never becomes a prompt
    /// on the child's event stream -- this arm is the safety net for a path
    /// that bypasses that, and it fails the task immediately rather than
    /// waiting forever for a human who is not watching this session
    /// (`docs/agent-explore-design.md` decision 4).
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
        let capped = matches!(
            self.terminal,
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByIterationCap))
        ) && self.has_usable_report();
        let message = self.failure_message();
        TaskReport {
            session_id: session_id.as_uuid().to_string(),
            description: description.into(),
            report: self.report,
            capped,
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
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByIterationCap))
                if self.has_usable_report() =>
            {
                None
            }
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByIterationCap)) => {
                Some(detail("the task session ran out of turns before finishing"))
            }
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByDoomLoop)) => {
                Some(detail("the task session repeated itself and was stopped"))
            }
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

/// Turns watcher failures into a terminal outcome so both task and MoA
/// callers can still retire their child and publish its result. Termination
/// and notification policy remain with the caller.
pub(super) fn watch_until_terminal(
    events: &Receiver<Event>,
    cancel: &Receiver<()>,
    on_activity: &mut dyn FnMut(Option<String>),
) -> Outcome {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        fold_until_terminal(events, cancel, on_activity)
    }))
    .unwrap_or_else(|payload| Outcome {
        terminal: Terminal::Panicked(panic_message(&*payload)),
        report: None,
        error: None,
    })
}

/// Folds the child's event stream until it reaches a terminal state or the
/// requesting session goes away. Pure over its receivers and the
/// `on_activity` callback, so the whole decision table above is
/// unit-testable by feeding a scripted event sequence: every time what a
/// live progress row would say changes (current tool, reasoning vs
/// tool-running), the observation is handed to `on_activity` as the child's
/// current activity (`None` = reasoning). Terminal emission is the caller's
/// — the fold only reports running observations.
fn fold_until_terminal(
    events: &Receiver<Event>,
    cancel: &Receiver<()>,
    on_activity: &mut dyn FnMut(Option<String>),
) -> Outcome {
    let mut report = None;
    let mut error = None;
    // The child's own user message is the boundary: everything committed
    // before it belongs to session startup, not to the answer.
    let mut turn_started = false;
    // Live-progress tracking: what the child was last observed doing, and
    // the last observation actually emitted (dedup — history-shaped events
    // like `MessageCommitted` must not re-emit an unchanged row).
    let mut activity: Option<String> = None;
    let mut emitted: Option<Option<String>> = None;

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
                    Event::ApprovalRequested(_)
                    | Event::StateChanged(SessionState::WaitingForApproval) => {
                        break Terminal::Approval;
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
                    // 47). Both are unreachable in a v1 task child (the
                    // host never sends `Initialize` after the user message,
                    // an approval that would need a human is refused rather
                    // than raised, every allowed tool is synchronous, and
                    // task children are never resumed) —
                    // but if they ever gain an async or approval-capable
                    // tool, this arm becomes a premature-completion hazard
                    // and must be revisited.
                    Event::StateChanged(SessionState::WaitingForUser) if turn_started => {
                        break match report {
                            Some(_) => Terminal::Completed,
                            None => Terminal::TurnEnded(None),
                        };
                    }
                    Event::StateChanged(SessionState::Terminated) | Event::Exited(_) => {
                        break Terminal::Terminated;
                    }
                    Event::Error(failure) => error = Some(failure.message),
                    // Live progress: track what the child is doing. The tool
                    // id of the latest requested call is the row's activity;
                    // back in `Running` the child is reasoning between tools.
                    Event::ToolCallRequested(request) => {
                        activity = Some(request.tool_id);
                    }
                    Event::StateChanged(SessionState::Running) => {
                        activity = None;
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
                    // terminal (and unreachable in a v1 task child -- no
                    // approval it raises ever reaches an operator; see the
                    // note above).
                    | Event::ApprovalResolved(_)
                    | Event::ContinueTurnRequested(_)
                    // Provider rate-limit pacing is not terminal.
                    | Event::ProviderRateLimited(_)
                    // Standing-agent memory events are not terminal for a
                    // task child (a standing role never spawns task children).
                    | Event::MemoryDigest(_)
                    | Event::MemoryCheckpointMissed
                    | Event::SessionInputSent { .. } | Event::EnvironmentReady { .. } | Event::EnvironmentActivated(_) | Event::EnvironmentActivationFailed(_) | Event::SessionResumed | Event::InputQueuePaused(_) | Event::InputStarted(_) | Event::InputAccepted(_) | Event::InputOutcome(_) | Event::DeliveryAcknowledged(_) | Event::MemorySeeded | Event::MoaPassStarted(_) => {}
                }
                if emitted.as_ref() != Some(&activity) {
                    on_activity(activity.clone());
                    emitted = Some(activity.clone());
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

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

/// Wall-clock now, epoch milliseconds — the launch timestamp every live
/// progress event carries, so a client can show elapsed time that survives
/// re-attach. `0` if the clock is before the epoch (never, in practice).
pub(super) fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
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
        capped: output
            .get("capped")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        message: output
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}
