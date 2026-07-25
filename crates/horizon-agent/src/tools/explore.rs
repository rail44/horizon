//! `agent.explore` (`docs/agent-explore-design.md`): delegate an open-ended
//! exploration to a parallel, read-only session sharing the requester's
//! workspace, and fold only its final report back into the requester's
//! history.
//!
//! **Why this exists.** A session's history is monotonic -- every tool
//! result is retransmitted with every later provider request -- and
//! exploration output dominates it. The fragments a grep/read sweep
//! produces do not need to live in the requesting session's history at all;
//! only the conclusion does. So the sweep runs in a session of its own,
//! whose history is discarded with it.
//!
//! **The seam.** This crate cannot spawn a session: hosting one is
//! `horizon-sessiond`'s job (`docs/agent-runtime-split-design.md`). So
//! [`ExplorationHost`] is a daemon-provided capability handle, installed on
//! `ToolSessionState` at session construction exactly like the recall
//! store, the network proxy, and the judge already are
//! (`ToolSessionState::with_exploration_host`). `None` -- every test
//! construction, and any future host that can't spawn peers -- degrades to
//! an actionable error result, never a silent no-op.
//!
//! **The wait is an event subscription**, deliberately (decision 5): the
//! host hands back the exploration's own event stream and this module folds
//! it until the turn ends, rather than inventing a bespoke return channel.
//! The future common abstraction is "subscribe to another agent session's
//! blocking and stop events", and this tool has to be expressible on it
//! without rework.
//!
//! **Asynchrony and cancellation** follow `bash`/`web`'s precedent exactly:
//! `start` returns [`Execution::Started`] immediately so the requester's
//! session loop stays responsive, a dedicated waiter thread folds the
//! exploration's events, and the eventual result arrives on the session's
//! `async_results` channel. Cancelling the requester's turn produces a
//! synthetic `ToolCallFinished` for this call, which reaches
//! [`cancel_if_running`] through `tools::processing` -- the same hook
//! `bash::kill_if_running` and `web::cancel_if_running` hang on -- and that
//! terminates the exploration session.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};

use crate::contract::{
    Event, MessageRole, SessionId, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
    TurnEndReason,
};
use crate::tools::state::{session_runtime, ToolSessionState};
use crate::tools::{Execution, ToolCompletion};

pub(crate) const TOOL_ID: &str = "agent.explore";

/// A running exploration session, as handed back by the daemon.
pub struct StartedExploration {
    /// The spawned session's own id. Returned to the requester for cost
    /// attribution -- it is the join key between the requester's
    /// `ToolCallRequested`/`ToolCallFinished` records and the exploration's
    /// own rows in the event log and DuckDB projection (decision 3).
    pub session_id: SessionId,
    /// Every event the exploration session emits, in order, from before it
    /// has emitted any (the host installs the subscription *before*
    /// spawning the session, so nothing is missed).
    pub events: Receiver<Event>,
}

/// The daemon capability `agent.explore` is built on: spawn a peer session,
/// subscribe to its events, terminate it. Implemented by `horizon-sessiond`
/// (`session::SessiondExplorationHost`) and installed on the requester's
/// `ToolSessionState`; the requester's own workspace root, provider, and
/// session id are baked into the implementation at construction, so this
/// trait stays as narrow as the tool actually needs.
///
/// "Peer, not child" (decision 2): the implementation must spawn the
/// exploration against the *same* workspace root as the requester -- an
/// isolated requester's worktree included -- with no isolation of its own
/// and no derivation-tree edge. Do not describe this relationship in
/// parent/child terms.
pub trait ExplorationHost: Send + Sync {
    /// Spawns a read-only exploration session and sends `prompt` as its
    /// first (and only) user message. `Err` carries a message suitable for
    /// the model to read as the tool's error result.
    fn start(&self, prompt: String) -> Result<StartedExploration, String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start,
    /// whether the exploration finished, failed, or was cancelled. A
    /// no-op for a session that has already ended on its own.
    fn terminate(&self, session_id: SessionId);
}

/// Starts an `agent.explore` call. Mirrors `execution::execute_tier1_bash`'s
/// shape: fold the `ToolRunning`/`ToolCallStarted` pair now (the caller does
/// that with [`Execution::Started`]), deliver the real result later on the
/// session's async completion channel. Every failure that is knowable right
/// here resolves synchronously as an ordinary error tool result instead --
/// an [`Execution::Auto`], so the provider gets its `ToolCallResult` and the
/// turn continues rather than stalling on a call that never finishes.
pub(crate) fn start(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let prompt = match prompt_from_input(&request.input) {
        Ok(prompt) => prompt,
        Err(message) => return synchronous_error(request, message, None),
    };
    let Some(runtime) = session_runtime(session_id) else {
        return synchronous_error(
            request,
            format!("`{TOOL_ID}` has no registered session runtime"),
            None,
        );
    };
    let Some(host) = tool_state.exploration_host() else {
        return synchronous_error(
            request,
            format!("`{TOOL_ID}` is not available in this session"),
            None,
        );
    };
    let started = match host.start(prompt) {
        Ok(started) => started,
        Err(message) => {
            return synchronous_error(
                request,
                format!("could not start an exploration session: {message}"),
                None,
            )
        }
    };

    let call_id = request.call_id.clone();
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded::<()>(1);
    let generation = next_generation();
    if let Some(replaced) = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            (session_id, call_id.clone()),
            RegisteredExploration {
                generation,
                cancel: cancel_tx,
            },
        )
    {
        let _ = replaced.cancel.try_send(());
    }

    let result_tx = runtime.async_results.clone();
    let explore_session_id = started.session_id;
    let events = started.events;
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            fold_until_terminal(&events, &cancel_rx)
        }))
        .unwrap_or_else(|payload| Outcome {
            terminal: Terminal::Panicked(panic_message(&*payload)),
            report: None,
            error: None,
        });
        // An exploration session is meaningless without its waiter, so it is
        // torn down on every path out of the fold -- completion, failure,
        // cancellation, and a panic in the fold itself.
        host.terminate(explore_session_id);
        if finish_registration(session_id, &call_id, generation) {
            let _ = result_tx.send(ToolCompletion::Finished(ToolCallResult::new(
                call_id,
                outcome.into_output(explore_session_id),
            )));
        }
    });

    Execution::Started(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
    ])
}

/// Cancels the exploration behind `call_id`, if this session has one in
/// flight. Called for every provider-originated `ToolCallFinished` (see
/// `tools::processing`) -- a cancelled turn's synthetic result is exactly
/// the signal that a still-running exploration must be terminated. Removing
/// the registration first is what makes the waiter's own
/// [`finish_registration`] return `false`, so a cancelled call never folds a
/// second, contradictory result on top of the synthetic one.
pub(crate) fn cancel_if_running(session_id: SessionId, call_id: &ToolCallId) {
    let registered = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(session_id, call_id.clone()));
    if let Some(registered) = registered {
        let _ = registered.cancel.try_send(());
    }
}

/// Cancels every exploration this session still has in flight -- called
/// from `unregister_session_runtime` when the requesting session itself
/// goes away, mirroring `web::cancel_session`.
pub(crate) fn cancel_session(session_id: SessionId) {
    let mut explorations = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let keys = explorations
        .keys()
        .filter(|(registered_session, _)| *registered_session == session_id)
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(registered) = explorations.remove(&key) {
            let _ = registered.cancel.try_send(());
        }
    }
}

fn prompt_from_input(input: &Value) -> Result<String, String> {
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| "`prompt` is required and must be a string".to_string())?;
    if prompt.trim().is_empty() {
        return Err("`prompt` must not be empty".to_string());
    }
    Ok(prompt.to_string())
}

fn synchronous_error(
    request: &ToolCallRequest,
    message: String,
    session_id: Option<SessionId>,
) -> Execution {
    let mut output = json!({
        "is_error": true,
        "message": message,
    });
    if let (Some(session_id), Some(map)) = (session_id, output.as_object_mut()) {
        map.insert(
            "session_id".to_string(),
            Value::String(session_id.as_uuid().to_string()),
        );
    }
    Execution::Auto(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(request.call_id.clone(), output)),
    ])
}

/// How an exploration's event stream ended.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Terminal {
    /// The exploration's turn ended normally -- the report is final.
    Completed,
    /// The turn ended some other way (failed, cancelled, or halted by the
    /// turn cap). Whatever report exists is still returned, flagged as an
    /// error so the requester knows to ask something narrower (decision 7).
    TurnEnded(TurnEndReason),
    /// The exploration parked on an approval. Unreachable by construction
    /// -- every tool in the explore role's allowlist is auto-allowed -- but
    /// if it ever happens the call fails immediately rather than waiting
    /// forever for a human who is not watching this session (decision 4).
    Approval,
    /// The session terminated without ending its turn.
    Terminated,
    /// The event stream ended without a terminal event -- the session's
    /// thread is gone.
    Disconnected,
    /// The requester's turn was cancelled, or its session ended.
    Cancelled,
    /// The fold itself unwound.
    Panicked(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Outcome {
    terminal: Terminal,
    /// The last assistant message committed after the exploration's own
    /// user message -- "the session's final assistant text" (decision 1).
    /// Messages committed before that (the provider's own initialization
    /// notice) and mid-turn narration superseded by a later message are
    /// deliberately not part of it.
    report: Option<String>,
    /// The most recent `Event::Error` message, used only to explain a
    /// failure the terminal reason alone doesn't.
    error: Option<String>,
}

impl Outcome {
    fn into_output(self, session_id: SessionId) -> Value {
        let mut output = json!({ "session_id": session_id.as_uuid().to_string() });
        let map = output.as_object_mut().expect("json object");
        let failure = self.failure_message();
        if let Some(report) = self.report {
            map.insert("report".to_string(), Value::String(report));
        }
        if let Some(message) = failure {
            map.insert("is_error".to_string(), Value::Bool(true));
            map.insert("message".to_string(), Value::String(message));
        }
        output
    }

    fn failure_message(&self) -> Option<String> {
        let detail = |suffix: &str| match &self.error {
            Some(error) => format!("{suffix} ({error})"),
            None => suffix.to_string(),
        };
        match &self.terminal {
            Terminal::Completed if self.report.is_some() => None,
            Terminal::Completed => Some(detail(
                "the exploration session ended its turn without producing a report",
            )),
            Terminal::TurnEnded(TurnEndReason::HaltedByIterationCap) => Some(detail(
                "the exploration session ran out of turns before finishing; ask a narrower \
                 question",
            )),
            Terminal::TurnEnded(TurnEndReason::HaltedByDoomLoop) => Some(detail(
                "the exploration session repeated itself and was stopped; ask a narrower question",
            )),
            Terminal::TurnEnded(reason) => Some(detail(&format!(
                "the exploration session's turn ended as {reason:?} before it finished"
            ))),
            Terminal::Approval => Some(
                "the exploration session asked for an approval it can never receive; nothing was \
                 run"
                .to_string(),
            ),
            Terminal::Terminated => Some(detail(
                "the exploration session terminated before finishing",
            )),
            Terminal::Disconnected => Some(detail("the exploration session stopped responding")),
            // A cancelled exploration never produces a completion at all
            // (`finish_registration` returns `false` for it), so this arm
            // exists only for exhaustiveness.
            Terminal::Cancelled => Some("the exploration was cancelled".to_string()),
            Terminal::Panicked(message) => {
                Some(format!("the exploration waiter panicked: {message}"))
            }
        }
    }
}

/// Folds the exploration's event stream until it reaches a terminal state or
/// the requester cancels. Pure over its two receivers, so the whole decision
/// table above is unit-testable by feeding a scripted event sequence.
fn fold_until_terminal(events: &Receiver<Event>, cancel: &Receiver<()>) -> Outcome {
    let mut report = None;
    let mut error = None;
    // The exploration's own user message is the boundary: everything
    // committed before it belongs to session startup, not to the answer.
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
                            other => Terminal::TurnEnded(other),
                        };
                    }
                    // Fallback end-of-turn signal for a provider that does
                    // not emit `TurnEnded` at all. The rig provider always
                    // emits it first (`apply_turn_outcome`), so this only
                    // ever fires for one that doesn't.
                    Event::StateChanged(SessionState::WaitingForUser) if turn_started => {
                        break match report {
                            Some(_) => Terminal::Completed,
                            None => Terminal::TurnEnded(TurnEndReason::Unknown),
                        };
                    }
                    Event::StateChanged(SessionState::Terminated) | Event::Exited(_) => {
                        break Terminal::Terminated;
                    }
                    Event::Error(failure) => error = Some(failure.message),
                    _ => {}
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

type ExplorationKey = (SessionId, ToolCallId);

struct RegisteredExploration {
    generation: u64,
    cancel: Sender<()>,
}

fn registry() -> &'static Mutex<HashMap<ExplorationKey, RegisteredExploration>> {
    static REGISTRY: OnceLock<Mutex<HashMap<ExplorationKey, RegisteredExploration>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_generation() -> u64 {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Whether this waiter's own registration is still the current one -- i.e.
/// nothing cancelled or superseded it while it waited. `false` means its
/// result must be dropped rather than folded, the same generation guard
/// `tools::web` uses for a provider call id reused across sessions.
fn finish_registration(session_id: SessionId, call_id: &ToolCallId, generation: u64) -> bool {
    let key = (session_id, call_id.clone());
    let mut explorations = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if explorations
        .get(&key)
        .is_some_and(|registered| registered.generation == generation)
    {
        explorations.remove(&key);
        true
    } else {
        false
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
