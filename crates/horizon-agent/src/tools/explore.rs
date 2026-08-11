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
//! **Asynchronous since 2026-07-28** (`docs/agent-async-task-design.md`,
//! owner-approved). [`start`] returns immediately with `{session_id,
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
//! (`ToolSessionState::with_exploration_host`). `None` -- every test
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

mod children;
mod notify;

use std::panic::AssertUnwindSafe;

use crossbeam_channel::Receiver;
use serde_json::{json, Value};

use crate::contract::{
    Event, MessageRole, SessionId, SessionState, ToolCallRequest, ToolCallResult, TurnEndReason,
};
use crate::tools::state::ToolSessionState;
use crate::tools::Execution;

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
    /// Spawns a read-only task session and sends `prompt` as its first user
    /// message -- the session's entire history; there is no other seeding.
    /// `Err` carries a message suitable for the model to read as the tool's
    /// error result.
    fn start(&self, prompt: String) -> Result<StartedExploration, String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start -- when
    /// the child's own turn ends, or when the requesting session goes away.
    /// A no-op for a session that has already ended on its own.
    fn terminate(&self, session_id: SessionId);
}

/// Launches a `task` child and returns immediately. Unlike every other
/// asynchronous tool in this crate (`bash`, `web`), the *tool call* itself
/// finishes here and now -- an [`Execution::Auto`] carrying the
/// `{session_id, description, status: "started"}` receipt -- because the
/// work it started is no longer part of this call. The child's eventual
/// report arrives on its own schedule as a notification (see the module
/// doc), not as this call's result.
pub(crate) fn start(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let input = match Input::parse(&request.input) {
        Ok(input) => input,
        Err(message) => return synchronous(request, error_output(message)),
    };
    let Some(host) = tool_state.exploration_host() else {
        return synchronous(
            request,
            error_output(format!("`{TOOL_ID}` is not available in this session")),
        );
    };

    let started = match host.start(input.prompt) {
        Ok(started) => started,
        Err(message) => {
            return synchronous(
                request,
                error_output(format!("could not start a task session: {message}")),
            )
        }
    };

    let child_id = started.session_id;
    let description = input.description;
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded::<()>(1);
    children::register(session_id, child_id, description.clone(), host, cancel_tx);

    let events = started.events;
    let waiter_description = description.clone();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            fold_until_terminal(&events, &cancel_rx)
        }))
        .unwrap_or_else(|payload| Outcome {
            terminal: Terminal::Panicked(panic_message(&*payload)),
            report: None,
            error: None,
        });
        // `Cancelled` means the requesting session went away and
        // `cancel_session` already terminated this child directly; anything
        // else means the child reached a terminal state of its own and this
        // is the one place that releases it. `take_host`'s take-once
        // semantics is what makes "terminate exactly once" hold either way.
        if outcome.terminal != Terminal::Cancelled {
            if let Some(host) = children::take_host(child_id) {
                host.terminate(child_id);
            }
        }
        let output = outcome.into_output(child_id, &waiter_description);
        if let Some(requester) = children::complete(child_id, output) {
            notify::wake(requester);
        }
    });

    synchronous(
        request,
        json!({
            "session_id": child_id.as_uuid().to_string(),
            "description": description,
            "status": "started",
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
    children::complete(child, output);
}

/// `task_output`: the full report of one finished child owned by this
/// session (decision 3). Ownership is checked, and an id belonging to
/// another session reports exactly like an unknown one -- a session must
/// not be able to probe another's task ids.
pub(crate) fn output(session_id: SessionId, request: &ToolCallRequest) -> Execution {
    let target = match request
        .input
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "`session_id` is required and must be a string".to_string())
        .and_then(|raw| {
            raw.trim()
                .parse::<uuid::Uuid>()
                .map(SessionId::from_uuid)
                .map_err(|_| format!("`{raw}` is not a task session id"))
        }) {
        Ok(target) => target,
        Err(message) => return synchronous(request, error_output(message)),
    };

    let output = match children::lookup(session_id, target) {
        children::Lookup::Unknown => error_output(format!(
            "no task with session_id `{}` was launched from this session",
            target.as_uuid()
        )),
        children::Lookup::Running { description } => json!({
            "session_id": target.as_uuid().to_string(),
            "description": description,
            "status": "running",
            "message": "this task has not finished yet. If nothing else is ready to \
                        do, end your turn instead of polling: the completion \
                        notification starts a new turn on its own, and calling \
                        task_output again before then only costs a round.",
        }),
        children::Lookup::Finished {
            description,
            mut output,
        } => {
            if let Some(map) = output.as_object_mut() {
                map.insert("description".to_string(), Value::String(description));
                map.insert("status".to_string(), Value::String("finished".to_string()));
            }
            output
        }
    };
    synchronous(request, output)
}

/// Terminates every child this session launched and drops its undelivered
/// notifications -- called from `unregister_session_runtime` when the
/// requesting session itself goes away. This is the *only* thing that kills
/// a child: a cancelled turn deliberately leaves them running (decision 4).
pub(crate) fn cancel_session(session_id: SessionId) {
    for child in children::take_all_for_requester(session_id) {
        let _ = child.cancel.try_send(());
        if let Some(host) = child.host {
            host.terminate(child.session_id);
        }
    }
    notify::unregister_wake(session_id);
}

/// Drains `session_id`'s finished-task queue into the single notification
/// message its next provider round should carry, or `None` when nothing is
/// waiting. Called by the rig session loop -- before each round, and again
/// when a wake signal arrives with the turn already ended.
pub(crate) fn take_notification(session_id: SessionId) -> Option<String> {
    let completions = children::take_pending(session_id);
    (!completions.is_empty()).then(|| notify::notification_text(&completions))
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
    if let Some(requester) = children::complete(child, output) {
        notify::wake(requester);
    }
}

/// What the model may say: a short display label and the question with the
/// exact deliverable wanted back.
struct Input {
    description: String,
    prompt: String,
}

impl Input {
    /// Validates both required fields. `description` is not forwarded to
    /// the child (which is seeded with `prompt` alone) -- it labels the
    /// requester's own transcript row while the task runs
    /// (`transcript::tool_call::classify`) and names the task in the
    /// completion notification.
    fn parse(input: &Value) -> Result<Self, String> {
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| "`description` is required and must be a string".to_string())?;
        if description.trim().is_empty() {
            return Err("`description` must not be empty".to_string());
        }
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| "`prompt` is required and must be a string".to_string())?;
        if prompt.trim().is_empty() {
            return Err("`prompt` must not be empty".to_string());
        }
        Ok(Self {
            description: description.trim().to_string(),
            prompt: prompt.to_string(),
        })
    }
}

/// A tool call that resolves right now -- which, since the 2026-07-28
/// asynchronous cutover, is *every* `task`/`task_output` call: launching is
/// no longer something the call waits on.
fn synchronous(request: &ToolCallRequest, output: Value) -> Execution {
    Execution::Auto(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            output,
        )),
    ])
}

use super::error_output;

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
fn report_body_is_empty(report: &str) -> bool {
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
enum Terminal {
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
    /// The child parked on an approval. Unreachable by construction -- every
    /// tool in the explore role's allowlist is auto-allowed -- but if it
    /// ever happens the task fails immediately rather than waiting forever
    /// for a human who is not watching this session
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
struct Outcome {
    terminal: Terminal,
    /// The last assistant message committed after the child's own user
    /// message -- "the session's final assistant text". Messages committed
    /// before that (the provider's own initialization notice) and mid-turn
    /// narration superseded by a later message are deliberately not part of
    /// it.
    report: Option<String>,
    /// The most recent `Event::Error` message, used only to explain a
    /// failure the terminal reason alone doesn't.
    error: Option<String>,
}

impl Outcome {
    fn into_output(self, session_id: SessionId, description: &str) -> Value {
        let mut output = json!({
            "session_id": session_id.as_uuid().to_string(),
            "description": description,
        });
        let map = output.as_object_mut().expect("json object");
        // A report alongside `HaltedByIterationCap` means the forced
        // wrap-up completion (`providers::rig::session::halt_turn_loop`)
        // succeeded -- a partial but genuine answer, not a failure. Flagged
        // explicitly so the requester (and anything downstream) can tell
        // "capped, still useful" apart from an ordinary completed report.
        let capped = matches!(
            self.terminal,
            Terminal::TurnEnded(Some(TurnEndReason::HaltedByIterationCap))
        ) && self.has_usable_report();
        let failure = self.failure_message();
        if let Some(report) = self.report {
            map.insert("report".to_string(), Value::String(report));
        }
        if capped {
            map.insert("capped".to_string(), Value::Bool(true));
        }
        if let Some(message) = failure {
            map.insert("is_error".to_string(), Value::Bool(true));
            map.insert("message".to_string(), Value::String(message));
        }
        output
    }

    /// A report with an actual body behind it -- a stored report that is
    /// nothing but a stray reasoning-close artifact does not count as one,
    /// so a child that shipped only that is reported as the failure it was
    /// (see [`report_body_is_empty`]).
    fn has_usable_report(&self) -> bool {
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

/// Folds the child's event stream until it reaches a terminal state or the
/// requesting session goes away. Pure over its two receivers, so the whole
/// decision table above is unit-testable by feeding a scripted event
/// sequence.
fn fold_until_terminal(events: &Receiver<Event>, cancel: &Receiver<()>) -> Outcome {
    let mut report = None;
    let mut error = None;
    // The child's own user message is the boundary: everything committed
    // before it belongs to session startup, not to the answer.
    let mut turn_started = false;

    let terminal = loop {
        crossbeam_channel::select! {
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
                    // approvals cannot occur, every allowed tool is
                    // synchronous, and task children are never resumed) —
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
                    // Guard-fail fallbacks: `TurnEnded` and
                    // `StateChanged(WaitingForUser)` above are guarded on
                    // `turn_started`; before the child's own user message that
                    // guard fails and they land here as no-ops (session
                    // startup, not an answer). `StateChanged(_)` also absorbs
                    // the non-terminal session states this watcher never acts
                    // on. Grouped so the match stays exhaustive without a
                    // wildcard -- adding a variant is a compile error here,
                    // forcing a decision (see docs/agent-event-readers.md).
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
                    // terminal (and unreachable in a v1 task child -- approvals
                    // cannot occur there; see the note above).
                    | Event::ApprovalResolved(_)
                    | Event::ContinueTurnRequested(_)
                    // Provider rate-limit pacing is not terminal.
                    | Event::ProviderRateLimited(_) => {}
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

#[cfg(test)]
mod tests;
