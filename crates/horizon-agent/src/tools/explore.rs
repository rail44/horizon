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
//!
//! **Lifetime (2026-07-26 addendum B).** An exploration session outlives the
//! turn it answered: a later call naming its `session_id` sends a follow-up
//! user message to that same session, which still holds the files it read in
//! its own history. The scope is the *requester's* turn -- `TurnEnded` on
//! the requesting session terminates every exploration it still has alive
//! (see [`terminate_session_explorations`], driven from `tools::processing`
//! next to the cancellation hook above), as does the requester's session
//! going away ([`cancel_session`]). The live-exploration map ([`live`]) is
//! the single owner of that teardown, so "terminate exactly once" is a
//! property of removing an entry from it rather than of any one code path's
//! care.
//!
//! **Fork seeding (2026-07-26 addendum C).** Under [`SeedMode::Fork`] the
//! spawned session's initial provider history is a copy of the requester's
//! own, reconstructed from the requester's live event stream (the same
//! event-to-history mapping session resume uses) and tail-sanitized by
//! [`sanitize_seed_history`] -- the requester's stream is captured mid-turn,
//! so tool calls still awaiting results can be sitting in it, and a history
//! carrying one of those is a history no provider will accept. The mode is
//! chosen by the harness, never by the model: `horizon-sessiond` reads
//! `HORIZON_EXPLORE_SEED` once per session and installs the result on
//! `ToolSessionState`.

use std::collections::{HashMap, HashSet};
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

pub(crate) const TOOL_ID: &str = "agent.explore";

/// Selects whether a fresh exploration starts empty or inherits a copy of
/// the requester's history (`docs/agent-explore-design.md`'s 2026-07-26
/// addendum C). Environment-only and harness-selected on purpose: a
/// model-visible parameter would confound the adoption measurement the two
/// modes exist to be compared by.
pub const SEED_MODE_VAR: &str = "HORIZON_EXPLORE_SEED";

/// What a fresh exploration session's provider history starts as. See
/// [`SEED_MODE_VAR`]; follow-up calls are unaffected (they continue a
/// session that already has whatever history it was started with).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SeedMode {
    /// The v1 behavior: the exploration starts with an empty history and
    /// sees only the prompt.
    #[default]
    Fresh,
    /// The exploration starts from a sanitized copy of the requester's
    /// history, so it sees the evidence the requester saw. Costs the
    /// requester's current context size out of the delegate's own window.
    Fork,
}

impl SeedMode {
    /// Reads [`SEED_MODE_VAR`]. The one place in the process that consults
    /// the environment for this: everything downstream takes the mode as a
    /// value, which is what keeps it testable without mutating a
    /// process-global.
    pub fn from_env() -> Self {
        match std::env::var(SEED_MODE_VAR) {
            Ok(raw) => Self::parse(&raw).unwrap_or_else(|| {
                eprintln!(
                    "horizon-agent: ignoring unrecognized {SEED_MODE_VAR}=`{raw}` (expected \
                     `fresh` or `fork`); exploration sessions start fresh"
                );
                Self::Fresh
            }),
            Err(_) => Self::Fresh,
        }
    }

    /// `None` for a value this build doesn't know -- the caller decides what
    /// to do about it. An empty/whitespace value reads as the default rather
    /// than as an error, matching how an unset variable behaves.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "fresh" => Some(Self::Fresh),
            "fork" => Some(Self::Fork),
            _ => None,
        }
    }
}

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
    /// first user message. `seed_history` is the exploration's initial
    /// provider history -- empty under [`SeedMode::Fresh`], a sanitized copy
    /// of the requester's own under [`SeedMode::Fork`]; the host is
    /// responsible only for handing it to the provider, never for producing
    /// or validating it. `Err` carries a message suitable for the model to
    /// read as the tool's error result.
    fn start(&self, prompt: String, seed_history: Vec<Event>)
        -> Result<StartedExploration, String>;

    /// Sends `prompt` as a further user message to an exploration session
    /// started earlier by [`Self::start`] and installs a fresh subscription
    /// on its events, so the caller can fold that session's *next* turn.
    /// `Err` for a session that is no longer running (or was never an
    /// exploration), which the tool surfaces as an ordinary error result.
    fn follow_up(
        &self,
        session_id: SessionId,
        prompt: String,
    ) -> Result<StartedExploration, String>;

    /// Terminates a session started by [`Self::start`] and releases its
    /// event subscription. Called exactly once per successful start --
    /// when the requester's turn ends, is cancelled, or the requesting
    /// session goes away. A no-op for a session that has already ended on
    /// its own.
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
    let input = match Input::parse(&request.input) {
        Ok(input) => input,
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

    let mut note = None;
    let started = match input.follow_up {
        Some(target) => {
            if !is_live(session_id, target) {
                return synchronous_error(request, no_such_exploration(target), Some(target));
            }
            match host.follow_up(target, input.prompt) {
                Ok(started) => started,
                Err(message) => {
                    // The session died between the last turn and this call:
                    // drop it from the live set so a repeat attempt fails the
                    // same way rather than routing to a corpse.
                    take_live(session_id, target);
                    return synchronous_error(
                        request,
                        format!("{message}; {FRESH_SPAWN_ALTERNATIVE}"),
                        Some(target),
                    );
                }
            }
        }
        None => {
            let (seed, seed_note) = seed_history(tool_state, &runtime.live_state);
            note = seed_note;
            match host.start(input.prompt, seed) {
                Ok(started) => started,
                Err(message) => {
                    return synchronous_error(
                        request,
                        format!("could not start an exploration session: {message}"),
                        None,
                    )
                }
            }
        }
    };
    register_live(session_id, started.session_id, host.clone());

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
        // Addendum B: an exploration whose turn merely *ended* outlives it,
        // so the requester can follow up on a session that still holds what
        // it read; the requester's own turn end -- or its cancellation,
        // already handled by `cancel_if_running` -- terminates it. One that
        // ended in a state it cannot answer from is torn down right here
        // instead: leaving it alive would let a follow-up park forever on a
        // session that will never emit another turn end, and the requester's
        // turn cannot end while that follow-up is in flight.
        if outcome.terminal.leaves_the_session_unusable() {
            if let Some(exploration) = take_live(session_id, explore_session_id) {
                exploration.terminate();
            }
        }
        if finish_registration(session_id, &call_id, generation) {
            let _ = result_tx.send(ToolCompletion::Finished(ToolCallResult::new(
                call_id,
                outcome.into_output(explore_session_id, note),
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
        if let Some(exploration) = take_live(session_id, registered.exploration_id) {
            exploration.terminate();
        }
    }
}

/// Terminates every exploration session `session_id` still has alive.
/// Called for the requester's own `Event::TurnEnded` (see
/// `tools::processing`) -- addendum B's lifetime rule: an exploration is
/// scoped to the turn that asked for it, whether that turn completed,
/// failed, or was halted, so no idle orphan survives into the next one.
pub(crate) fn terminate_session_explorations(session_id: SessionId) {
    for exploration in take_all_live(session_id) {
        exploration.terminate();
    }
}

/// Cancels every exploration this session still has in flight and
/// terminates every one it still has alive -- called from
/// `unregister_session_runtime` when the requesting session itself goes
/// away, mirroring `web::cancel_session`.
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
    drop(explorations);
    terminate_session_explorations(session_id);
}

/// What the model may say. `session_id` turns the call into a follow-up on
/// an exploration this session already started (addendum B); absent, the
/// call spawns a fresh one, which is v1's only behavior.
struct Input {
    prompt: String,
    follow_up: Option<SessionId>,
}

impl Input {
    fn parse(input: &Value) -> Result<Self, String> {
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| "`prompt` is required and must be a string".to_string())?;
        if prompt.trim().is_empty() {
            return Err("`prompt` must not be empty".to_string());
        }
        let follow_up = match input.get("session_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                Some(SessionId::from_uuid(raw.trim().parse().map_err(|_| {
                    format!("`session_id` is not a session id; {FRESH_SPAWN_ALTERNATIVE}")
                })?))
            }
            Some(_) => {
                return Err(format!(
                    "`session_id` must be the id string returned by an earlier call; \
                     {FRESH_SPAWN_ALTERNATIVE}"
                ))
            }
        };
        Ok(Self {
            prompt: prompt.to_string(),
            follow_up,
        })
    }
}

const FRESH_SPAWN_ALTERNATIVE: &str =
    "omit `session_id` to start a fresh exploration for this question";

fn no_such_exploration(session_id: SessionId) -> String {
    format!(
        "no exploration session `{}` is alive for this session (it was never started here, or it \
         ended with an earlier turn); {FRESH_SPAWN_ALTERNATIVE}",
        session_id.as_uuid()
    )
}

/// The initial provider history a fresh exploration is spawned with, plus a
/// note for the tool result when a requested fork could not be honored.
/// Seeding never fails the call: an empty seed degrades to exactly the
/// [`SeedMode::Fresh`] behavior, said out loud so a measurement run isn't
/// silently mis-attributed to the wrong arm.
fn seed_history(
    tool_state: &ToolSessionState,
    live_state: &crate::live::LiveState,
) -> (Vec<Event>, Option<String>) {
    if tool_state.exploration_seed_mode() != SeedMode::Fork {
        return (Vec::new(), None);
    }
    let seed = sanitize_seed_history(&live_state.events());
    if seed.is_empty() {
        return (
            Vec::new(),
            Some(
                "this session had no history to copy, so the exploration started fresh".to_string(),
            ),
        );
    }
    (seed, None)
}

/// Makes `events` safe to replay as another session's initial provider
/// history (addendum C's mandatory tail sanitization).
///
/// The requester's stream is captured mid-turn, so it can end with tool
/// calls that have no result: an asynchronous `bash` or `web` call still
/// running, or another member of the same parallel batch this
/// `agent.explore` call arrived in. Mapped naively (`providers::rig::
/// mapping::rig_messages_from_horizon_events`) each becomes an assistant
/// tool-call message with no matching tool result, which providers reject
/// outright. (The `agent.explore` call being served is *not* among them, as
/// it happens: `horizon-sessiond`'s `handle_provider_event` folds a
/// processed batch into `LiveState` only after `tools::processing` returns,
/// so this runs one fold before its own request lands. That ordering is not
/// something to rely on -- it is exactly the kind of invariant that quietly
/// inverts -- so the unpaired tail is handled generally rather than by
/// special-casing one call id.)
///
/// The shape chosen here is **drop, not close**: an unpaired
/// `ToolCallRequested` is removed rather than given a synthetic result. A
/// synthetic result would have to invent what the call returned -- and for
/// the in-flight `agent.explore` call the honest answer is "the session
/// reading this", which is neither useful context nor something to steer the
/// delegate with. Dropping loses nothing the delegate can act on and makes
/// the postcondition provable: every surviving call has exactly one
/// surviving result, in that order. Orphan results (a `ToolCallFinished`
/// with no request before it) and duplicate members of either kind are
/// dropped for the same reason. Everything else -- messages, errors --
/// passes through untouched, so a fork sees exactly what a resume of the
/// requester would.
pub(crate) fn sanitize_seed_history(events: &[Event]) -> Vec<Event> {
    let mut dropped: HashSet<usize> = HashSet::new();
    let mut pending: HashMap<ToolCallId, usize> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        match event {
            Event::ToolCallRequested(request) => {
                // A second request for a call id still awaiting its result
                // supersedes the first, which can now never be paired.
                if let Some(superseded) = pending.insert(request.call_id.clone(), index) {
                    dropped.insert(superseded);
                }
            }
            Event::ToolCallFinished(result) if pending.remove(&result.call_id).is_none() => {
                dropped.insert(index);
            }
            _ => {}
        }
    }
    dropped.extend(pending.into_values());

    events
        .iter()
        .enumerate()
        .filter(|(index, _)| !dropped.contains(index))
        .map(|(_, event)| event.clone())
        .collect()
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

impl Terminal {
    /// Whether the exploration session is in no state to answer a follow-up
    /// -- either it is gone, or it is parked somewhere no further turn end
    /// will ever come from. The waiter tears those down immediately rather
    /// than leaving them for the requester's turn end (see its call site:
    /// a follow-up onto one of these would block the very turn end that
    /// would have cleaned it up). `TurnEnded` is deliberately absent: a
    /// turn that ended -- failed, cancelled, or capped -- leaves the session
    /// back at `WaitingForUser`, and following up on a capped exploration
    /// with a narrower question is exactly the repair decision 7 asks for.
    fn leaves_the_session_unusable(&self) -> bool {
        match self {
            Self::Approval | Self::Terminated | Self::Disconnected | Self::Panicked(_) => true,
            // Cancelled has already been torn down by `cancel_if_running`.
            Self::Completed | Self::TurnEnded(_) | Self::Cancelled => false,
        }
    }
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
    fn into_output(self, session_id: SessionId, note: Option<String>) -> Value {
        let mut output = json!({ "session_id": session_id.as_uuid().to_string() });
        let map = output.as_object_mut().expect("json object");
        let failure = self.failure_message();
        if let Some(note) = note {
            map.insert("note".to_string(), Value::String(note));
        }
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

/// One exploration session that is still alive, with the host that can
/// terminate it.
struct LiveExploration {
    session_id: SessionId,
    host: Arc<dyn ExplorationHost>,
}

impl LiveExploration {
    fn terminate(self) {
        self.host.terminate(self.session_id);
    }
}

/// Every exploration session still alive, grouped by the session that
/// started it (addendum B). Separate from [`registry`], which tracks
/// *in-flight calls*: an exploration outlives the call that spawned it and
/// can serve several of them, so ownership of its teardown has to live
/// somewhere the call's waiter thread does not.
///
/// This is the single place a `terminate` originates from. Every path
/// (turn end, cancellation, requester death, an unusable terminal state)
/// removes the entry first and terminates second, so "exactly once" holds
/// without any path having to know about the others.
fn live() -> &'static Mutex<HashMap<SessionId, Vec<LiveExploration>>> {
    static LIVE: OnceLock<Mutex<HashMap<SessionId, Vec<LiveExploration>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_live() -> std::sync::MutexGuard<'static, HashMap<SessionId, Vec<LiveExploration>>> {
    live()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn register_live(requester: SessionId, exploration: SessionId, host: Arc<dyn ExplorationHost>) {
    let mut live = lock_live();
    let entries = live.entry(requester).or_default();
    if !entries.iter().any(|entry| entry.session_id == exploration) {
        entries.push(LiveExploration {
            session_id: exploration,
            host,
        });
    }
}

fn is_live(requester: SessionId, exploration: SessionId) -> bool {
    lock_live()
        .get(&requester)
        .is_some_and(|entries| entries.iter().any(|entry| entry.session_id == exploration))
}

fn take_live(requester: SessionId, exploration: SessionId) -> Option<LiveExploration> {
    let mut live = lock_live();
    let entries = live.get_mut(&requester)?;
    let position = entries
        .iter()
        .position(|entry| entry.session_id == exploration)?;
    let taken = entries.remove(position);
    if entries.is_empty() {
        live.remove(&requester);
    }
    Some(taken)
}

fn take_all_live(requester: SessionId) -> Vec<LiveExploration> {
    lock_live().remove(&requester).unwrap_or_default()
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
