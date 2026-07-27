//! `task` (`docs/agent-explore-design.md`): delegate an open-ended
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
//!
//! **One-shot, spawn-and-wait (2026-07-27 owner decision).** Follow-up
//! turns and fork seeding -- the 2026-07-26 addendum B/C measurement pair
//! -- were removed: both got in the way of fixing turn-folding behavior and
//! discussing its design, and the fork arm was separately shown to break
//! outright against at least one model's chat template. This tool's
//! input is the exploration prompt (plus a display label -- see [`Input`]);
//! every call spawns a fresh session
//! seeded with nothing but that prompt, and the waiter thread below
//! terminates it as soon as its own event stream reaches a terminal state
//! (see [`take_live`]'s call sites) -- no exploration ever outlives the call
//! that spawned it. See `docs/agent-explore-design.md`'s dated addendum for
//! the full record.
//!
//! **Iteration-cap exhaustion is a partial success, not an error.** A role
//! that opts in (`RoleDefinition::summarize_on_cap`, set for
//! `EXPLORE_ROLE` -- `docs/research/agent-context-reduction-prior-
//! art-2026-07-26.md` §4's OpenCode/Hermes precedent) gets one forced,
//! tools-disabled completion before the rig turn loop halts on the cap
//! (`providers::rig::session::halt_turn_loop`), so the exploration reports
//! whatever it found instead of discarding the work behind a generic error.
//! [`Outcome::into_output`] surfaces that as an ordinary (`is_error: false`)
//! result carrying a `capped: true` marker, not a failure.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};

use crate::contract::{
    Event, MessageRole, SessionId, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
    TurnEndReason,
};
use crate::tools::state::{session_runtime, ToolSessionState};
use crate::tools::{Execution, ToolCompletion};

/// The model-visible tool id. Renamed from `agent.explore` on 2026-07-27:
/// the plain `task` name measurably improved delegation adoption for one of
/// the two production models and was neutral for the other
/// (`docs/research/agent-delegation-and-batching-probes-2026-07-27.md`,
/// cell C3).
///
/// Deliberately *not* the same string as `roles::EXPLORE_ROLE_ID`, which
/// stays `"explore"`. That one is a persistence and cleanup identity -- it
/// is written into the event log, decides which sessions
/// `horizon-sessiond` refuses to resume at startup, and filters the
/// client-visible session list -- so renaming it would touch resume paths
/// and already-persisted records while buying nothing the model can see.
pub(crate) const TOOL_ID: &str = "task";

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

/// The daemon capability `task` is built on: spawn a peer session,
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
    /// first user message -- the session's entire history; there is no
    /// other seeding (2026-07-27: fork seeding was removed, see the module
    /// doc). `Err` carries a message suitable for the model to read as the
    /// tool's error result.
    fn start(&self, prompt: String) -> Result<StartedExploration, String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start -- as
    /// soon as its own turn ends, the requester's call is cancelled, or the
    /// requesting session goes away. A no-op for a session that has already
    /// ended on its own.
    fn terminate(&self, session_id: SessionId);
}

/// Starts a `task` call. Mirrors `execution::execute_tier1_bash`'s
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
    let input = match Input::parse(&request.input) {
        Ok(input) => input,
        Err(message) => return synchronous_error(request, message),
    };
    let Some(runtime) = session_runtime(session_id) else {
        return synchronous_error(
            request,
            format!("`{TOOL_ID}` has no registered session runtime"),
        );
    };
    let Some(host) = tool_state.exploration_host() else {
        return synchronous_error(
            request,
            format!("`{TOOL_ID}` is not available in this session"),
        );
    };

    let started = match host.start(input.prompt) {
        Ok(started) => started,
        Err(message) => {
            return synchronous_error(
                request,
                format!("could not start an exploration session: {message}"),
            )
        }
    };
    register_live(started.session_id, host.clone());

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
                exploration_id: started.session_id,
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
        // One-shot, spawn-and-wait: nothing outlives the call that spawned
        // it, so the exploration is torn down the moment its own event
        // stream reaches a terminal state. `Cancelled` is the one
        // exception -- `cancel_if_running` already terminated it directly
        // when it sent the cancel signal that produced this outcome, so
        // terminating here too would be a redundant (if harmless) second
        // call; skipping it keeps "exactly one terminate per exploration"
        // legible from `take_live`'s take-once semantics alone.
        if outcome.terminal != Terminal::Cancelled {
            if let Some(host) = take_live(explore_session_id) {
                host.terminate(explore_session_id);
            }
        }
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
/// flight, and terminates the session it was waiting on. Called for every
/// provider-originated `ToolCallFinished` (see `tools::processing`) -- a
/// cancelled turn's synthetic result is exactly the signal that a
/// still-running exploration is unwanted. Removing the registration first is
/// what makes the waiter's own [`finish_registration`] return `false`, so a
/// cancelled call never folds a second, contradictory result on top of the
/// synthetic one.
pub(crate) fn cancel_if_running(session_id: SessionId, call_id: &ToolCallId) {
    let registered = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(session_id, call_id.clone()));
    if let Some(registered) = registered {
        let _ = registered.cancel.try_send(());
        if let Some(host) = take_live(registered.exploration_id) {
            host.terminate(registered.exploration_id);
        }
    }
}

/// Cancels every exploration this session still has in flight and
/// terminates the session each one was waiting on -- called from
/// `unregister_session_runtime` when the requesting session itself goes
/// away, mirroring `web::cancel_session`. A still-live (not yet
/// self-terminated) exploration whose call was never cancelled by this
/// requester's own turn ending normally is the case this exists for: the
/// requester is gone, so nothing will ever fold that call's result.
pub(crate) fn cancel_session(session_id: SessionId) {
    let mut explorations = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let keys = explorations
        .keys()
        .filter(|(registered_session, _)| *registered_session == session_id)
        .cloned()
        .collect::<Vec<_>>();
    let mut exploration_ids = Vec::new();
    for key in keys {
        if let Some(registered) = explorations.remove(&key) {
            let _ = registered.cancel.try_send(());
            exploration_ids.push(registered.exploration_id);
        }
    }
    drop(explorations);
    for exploration_id in exploration_ids {
        if let Some(host) = take_live(exploration_id) {
            host.terminate(exploration_id);
        }
    }
}

/// What the model may say: a short display label and the exploration
/// question with the exact deliverable wanted back. Nothing else -- there is
/// no way to target an existing exploration session (2026-07-27: follow-up
/// was removed, see the module doc); every call spawns a fresh one.
struct Input {
    prompt: String,
}

impl Input {
    /// Validates both required fields but keeps only `prompt`: `description`
    /// is a label for the human watching, not an input to the exploration
    /// (which is seeded with `prompt` alone). It reaches the UI from the
    /// persisted call input instead -- `transcript::tool_call::classify`
    /// renders it as this call's target. It is still validated here so the
    /// catalog schema's `required` is not a claim nothing enforces.
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
            prompt: prompt.to_string(),
        })
    }
}

fn synchronous_error(request: &ToolCallRequest, message: String) -> Execution {
    Execution::Auto(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            json!({
                "is_error": true,
                "message": message,
            }),
        )),
    ])
}

/// How an exploration's event stream ended.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Terminal {
    /// The exploration's turn ended normally -- the report is final.
    Completed,
    /// The turn ended some other way (failed, cancelled, or halted by a
    /// guard). Whatever report exists is still returned. For
    /// `HaltedByIterationCap` specifically, a report here means the rig turn
    /// loop's forced wrap-up completion succeeded (`RoleDefinition::
    /// summarize_on_cap`) -- see [`Outcome::failure_message`].
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
        // A report alongside `HaltedByIterationCap` means the forced
        // wrap-up completion (`providers::rig::session::halt_turn_loop`)
        // succeeded -- a partial but genuine answer, not a failure. Flagged
        // explicitly so the requester (and anything downstream) can tell
        // "capped, still useful" apart from an ordinary completed report.
        let capped = matches!(
            self.terminal,
            Terminal::TurnEnded(TurnEndReason::HaltedByIterationCap)
        ) && self.report.is_some();
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
            Terminal::TurnEnded(TurnEndReason::HaltedByIterationCap) if self.report.is_some() => {
                None
            }
            Terminal::TurnEnded(TurnEndReason::HaltedByIterationCap) => Some(detail(
                "the exploration session ran out of turns before finishing",
            )),
            Terminal::TurnEnded(TurnEndReason::HaltedByDoomLoop) => Some(detail(
                "the exploration session repeated itself and was stopped",
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
                    // ever fires for one that doesn't. One caveat kept on
                    // record: rig ALSO emits `WaitingForUser` mid-turn in
                    // two measured shapes (the startup/`Initialize` pair,
                    // and approval-gated async-tool boundaries — backlog
                    // 47). Both are unreachable in a v1 exploration (the
                    // host never sends `Initialize` after the user message,
                    // approvals cannot occur, every allowed tool is
                    // synchronous, and explorations are never resumed) —
                    // but if explorations ever gain an async or
                    // approval-capable tool, this arm becomes a premature-
                    // completion hazard and must be revisited.
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
    /// The session this call's waiter is folding, so cancelling the call can
    /// terminate it without going through the waiter thread.
    exploration_id: SessionId,
}

fn registry() -> &'static Mutex<HashMap<ExplorationKey, RegisteredExploration>> {
    static REGISTRY: OnceLock<Mutex<HashMap<ExplorationKey, RegisteredExploration>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Every exploration session still alive, keyed by its own session id. The
/// single "who terminates this" gate: registered right after `start()`, and
/// taken at most once -- by whichever of the waiter thread's own fold
/// completion, [`cancel_if_running`], or [`cancel_session`] gets there
/// first -- so "terminate exactly once" holds regardless of which of those
/// races.
fn live() -> &'static Mutex<HashMap<SessionId, Arc<dyn ExplorationHost>>> {
    static LIVE: OnceLock<Mutex<HashMap<SessionId, Arc<dyn ExplorationHost>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_live(exploration: SessionId, host: Arc<dyn ExplorationHost>) {
    live()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(exploration)
        .or_insert(host);
}

fn take_live(exploration: SessionId) -> Option<Arc<dyn ExplorationHost>> {
    live()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&exploration)
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
