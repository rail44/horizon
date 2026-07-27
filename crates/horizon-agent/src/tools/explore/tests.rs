use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::contract::{Error as AgentError, Message, ProviderEvent};
use crate::live::LiveState;
use crate::tools::execution::HostTools;
use crate::tools::processing::process_agent_provider_event;
use crate::tools::state::{register_session_runtime, unregister_session_runtime};

/// How long a test waits for the waiter thread to fold a scripted event
/// stream. Generous relative to the work (a handful of channel hops) so a
/// loaded machine never flakes this suite.
const WAIT: Duration = Duration::from_secs(10);

struct NoHostTools;

impl HostTools for NoHostTools {
    fn execute_auto(
        &self,
        _tool_id: &str,
        _input: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        None
    }
}

/// A scripted stand-in for `horizon-sessiond`'s real exploration host: it
/// hands the tool one half of a channel the test drives event by event, and
/// records every `terminate` call so the cancellation and teardown
/// requirements are directly assertable. Deterministic and offline -- no
/// provider, no session thread, nothing to wait on but the tool's own fold.
///
/// Each `start` mints a fresh channel and publishes its sending half on
/// `turns`, so a test can drive that turn's events independently.
struct ScriptedHost {
    session_id: SessionId,
    /// The sending half of every turn handed out so far, in order.
    turns: Mutex<Vec<Sender<Event>>>,
    started: Mutex<Vec<String>>,
    terminated: Arc<Mutex<Vec<SessionId>>>,
    start_error: Option<String>,
}

impl ScriptedHost {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<SessionId>>>) {
        let terminated = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Self {
            session_id: SessionId::new(),
            turns: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            terminated: terminated.clone(),
            start_error: None,
        });
        (host, terminated)
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: SessionId::new(),
            turns: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            terminated: Arc::new(Mutex::new(Vec::new())),
            start_error: Some(message.to_string()),
        })
    }

    /// The event sender for the `index`-th turn this host handed out,
    /// waiting briefly for it to exist (the tool registers it synchronously,
    /// but the test may drive events from before its own `request` returns).
    fn turn(&self, index: usize) -> Sender<Event> {
        self.turns
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("turn {index} was never started"))
    }

    fn open_turn(&self) -> Receiver<Event> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.turns.lock().unwrap().push(tx);
        rx
    }
}

impl ExplorationHost for ScriptedHost {
    fn start(&self, prompt: String) -> Result<StartedExploration, String> {
        if let Some(error) = &self.start_error {
            return Err(error.clone());
        }
        self.started.lock().unwrap().push(prompt);
        Ok(StartedExploration {
            session_id: self.session_id,
            events: self.open_turn(),
        })
    }

    fn terminate(&self, session_id: SessionId) {
        self.terminated.lock().unwrap().push(session_id);
    }
}

/// A requester session wired exactly as `horizon-sessiond`'s `run_session`
/// wires one: a registered runtime whose `async_results` channel is where
/// the exploration's eventual result lands.
struct Requester {
    session_id: SessionId,
    tool_state: ToolSessionState,
    live_state: LiveState,
    results: Receiver<ToolCompletion>,
}

impl Requester {
    fn new(host: Option<Arc<dyn ExplorationHost>>) -> Self {
        let session_id = SessionId::new();
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_exploration_host(host);
        let live_state = LiveState::with_disabled_persistence();
        let (results_tx, results) = crossbeam_channel::unbounded();
        register_session_runtime(
            session_id,
            tool_state.clone(),
            live_state.clone(),
            results_tx,
        );
        Self {
            session_id,
            tool_state,
            live_state,
            results,
        }
    }

    /// Runs one `task` call through the same entry point the
    /// session loop uses, folding whatever it produces into this session's
    /// live state (as `horizon-sessiond`'s `handle_provider_event` does).
    fn request(&self, call_id: &str, input: serde_json::Value) -> ToolCallId {
        let call_id = ToolCallId(call_id.to_string());
        let processing = process_agent_provider_event(
            &NoHostTools,
            &self.tool_state,
            self.session_id,
            ProviderEvent::from(Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: TOOL_ID.to_string(),
                input: input.into(),
            })),
        );
        self.live_state
            .extend_provider_events(processing.horizon_events);
        call_id
    }

    /// Folds an asynchronous completion the way sessiond's
    /// `fold_finished_bash_result` does, so the requester's history ends up
    /// in exactly the shape production would produce.
    fn fold(&self, result: ToolCallResult) {
        self.live_state
            .extend_provider_events(std::iter::once(Event::ToolCallFinished(result).into()));
    }

    /// Pushes an arbitrary provider event through the same entry point the
    /// session loop uses -- how a test reaches the turn-end and
    /// cancellation hooks in `tools::processing`.
    fn provider_event(&self, event: Event) {
        let processing = process_agent_provider_event(
            &NoHostTools,
            &self.tool_state,
            self.session_id,
            ProviderEvent::from(event),
        );
        self.live_state
            .extend_provider_events(processing.horizon_events);
    }

    fn await_result(&self) -> ToolCallResult {
        match self.results.recv_timeout(WAIT) {
            Ok(ToolCompletion::Finished(result)) => result,
            Ok(other) => panic!("expected a finished exploration completion, got {other:?}"),
            Err(error) => panic!("no exploration completion arrived: {error}"),
        }
    }
}

impl Drop for Requester {
    fn drop(&mut self) {
        unregister_session_runtime(self.session_id);
    }
}

fn user(text: &str) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::User,
        text: text.to_string(),
    })
}

fn assistant(text: &str) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::Assistant,
        text: text.to_string(),
    })
}

/// A whole successful exploration, end to end through the real tool entry
/// point: the requester's history ends up holding exactly the call and the
/// final report -- nothing the exploration read along the way -- and the
/// exploration session is terminated the moment its own turn ends
/// (`docs/agent-explore-design.md` decisions 1 and 3, and the 2026-07-27
/// one-shot addendum).
#[test]
fn a_completed_exploration_returns_only_its_final_report() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    let call_id = requester.request(
        "explore-1",
        json!({ "description": "map a call site", "prompt": "where are the WaitingForUser emissions?" }),
    );
    let events = host.turn(0);

    // Everything a real exploration would emit: startup chatter, its own
    // user message, a full read/grep sweep, then the report.
    events.send(assistant("Rig provider initialized.")).unwrap();
    events.send(user("where are the emissions?")).unwrap();
    events
        .send(Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("inner-grep".to_string()),
            tool_id: "fs.grep".to_string(),
            input: json!({ "pattern": "WaitingForUser" }).into(),
        }))
        .unwrap();
    events
        .send(Event::ToolCallFinished(ToolCallResult::new(
            ToolCallId("inner-grep".to_string()),
            json!({ "matches": ["one 200-line dump of locations"] }),
        )))
        .unwrap();
    events.send(assistant("Let me read the two hits.")).unwrap();
    events
        .send(assistant(
            "Emitted at session.rs:1747; consumed at frame.rs:88.",
        ))
        .unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();

    let result = requester.await_result();
    assert_eq!(result.call_id, call_id);
    assert!(!result.is_error, "unexpected error result: {result:?}");
    assert_eq!(
        result.output["report"],
        json!("Emitted at session.rs:1747; consumed at frame.rs:88."),
        "only the exploration's *final* assistant message is the deliverable"
    );
    assert_eq!(
        result.output["session_id"],
        json!(explore_session_id.as_uuid().to_string()),
        "the spawned session id must come back for cost attribution"
    );

    // One-shot, spawn-and-wait: termination happens in the same waiter
    // thread, strictly before the result is handed back, so it has already
    // happened by the time `await_result` returns.
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "the exploration must be terminated as soon as its own turn ends"
    );

    requester.fold(result.clone());
    let history = requester.live_state.events();
    assert!(
        history.iter().any(
            |event| matches!(event, Event::ToolCallRequested(request) if request.call_id == call_id)
        ),
        "the requester's history must record the call itself: {history:?}"
    );
    let serialized = serde_json::to_string(&history).expect("serialize requester history");
    assert!(
        !serialized.contains("one 200-line dump of locations"),
        "no exploration tool output may reach the requester's history: {serialized}"
    );
    assert!(
        !serialized.contains("Let me read the two hits."),
        "no mid-exploration narration may reach the requester's history: {serialized}"
    );
    assert!(
        !serialized.contains("inner-grep"),
        "no exploration tool call may reach the requester's history: {serialized}"
    );
    assert!(serialized.contains("session.rs:1747"), "{serialized}");
}

/// `cancel_session` (driven from `unregister_session_runtime`, when the
/// requesting session itself goes away) terminates every exploration this
/// requester still has a call in flight for. This is the case that matters
/// once a *completed* exploration already tears itself down on its own: a
/// still-running one is the only kind left that needs this path.
#[test]
fn the_requester_session_going_away_terminates_a_still_running_exploration() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-1",
        json!({ "description": "map a call site", "prompt": "look around" }),
    );
    let events = host.turn(0);
    events.send(user("look around")).unwrap();
    events.send(assistant("still searching...")).unwrap();
    // No `TurnEnded` yet -- the exploration is still running.
    assert!(terminated.lock().unwrap().is_empty());

    // What `Requester::drop` does, and what `spawn_session_thread`'s cleanup
    // does in production. `cancel_session` terminates the still-running
    // exploration synchronously, without waiting on its waiter thread.
    unregister_session_runtime(requester.session_id);

    assert_eq!(*terminated.lock().unwrap(), vec![explore_session_id]);
}

/// Decision 6: cancelling the requester's turn terminates the exploration
/// session. The provider's synthetic `ToolCallFinished` for the pending
/// call is the cancellation signal, exactly as it is for `bash`/`web`.
#[test]
fn cancelling_the_requesters_turn_terminates_the_exploration_session() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    let call_id = requester.request(
        "explore-cancel",
        json!({ "description": "map a call site", "prompt": "keep looking" }),
    );
    let events = host.turn(0);
    events.send(user("keep looking")).unwrap();
    events.send(assistant("still searching...")).unwrap();

    // What a cancelled turn produces for a still-pending tool call, followed
    // by the turn's own end.
    requester.provider_event(Event::ToolCallFinished(
        crate::tools::cancelled_tool_call_result(call_id.clone()),
    ));
    requester.provider_event(Event::TurnEnded(TurnEndReason::Cancelled));

    let deadline = std::time::Instant::now() + WAIT;
    while terminated.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "cancelling the requester's turn must terminate the exploration session exactly once, \
         even though the turn's own end follows the cancelled call's result"
    );
    assert!(
        requester
            .results
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "a cancelled exploration must not fold a second, contradictory result"
    );
}

/// Decision 4: approvals are unreachable for an exploration by
/// construction, but if one ever surfaces the call fails immediately rather
/// than waiting forever on a human who is not watching that session.
#[test]
fn an_approval_in_the_exploration_fails_the_call_immediately() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-approval",
        json!({ "description": "map a call site", "prompt": "look around" }),
    );
    let events = host.turn(0);
    events.send(user("look around")).unwrap();
    events
        .send(Event::StateChanged(SessionState::WaitingForApproval))
        .unwrap();

    let result = requester.await_result();
    assert!(
        result.is_error,
        "an approval must fail the call: {result:?}"
    );
    assert!(
        result.output["message"]
            .as_str()
            .expect("a message")
            .contains("approval"),
        "{result:?}"
    );
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "a session parked on an approval can never end another turn, so it is torn down at once"
    );
}

/// Success-with-caveats: a role that opts into the forced cap wrap-up
/// (`RoleDefinition::summarize_on_cap`, set for `EXPLORE_ROLE` --
/// `providers::rig::session::halt_turn_loop`/`run_cap_summary_turn`) has
/// already committed its summary as an ordinary assistant message by the
/// time this event stream reaches `TurnEnded(HaltedByIterationCap)` -- so a
/// report here is the expected, common case, and the call succeeds with an
/// explicit `capped` marker rather than erroring with the work discarded
/// (`docs/agent-explore-design.md`'s 2026-07-27 addendum;
/// `docs/research/agent-context-reduction-prior-art-2026-07-26.md` §4).
#[test]
fn a_capped_exploration_with_a_forced_summary_is_a_partial_success() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-capped",
        json!({ "description": "map a call site", "prompt": "map the module" }),
    );
    let events = host.turn(0);
    events.send(user("map the module")).unwrap();
    events
        .send(assistant("Let me check the last two call sites."))
        .unwrap();
    // The forced wrap-up completion's summary, committed before the turn
    // ends -- see `run_cap_summary_turn`.
    events
        .send(assistant(
            "Found call sites in a.rs:10 and b.rs:22; c.rs and d.rs remain unchecked.",
        ))
        .unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::HaltedByIterationCap))
        .unwrap();

    let result = requester.await_result();
    assert!(
        !result.is_error,
        "a capped-but-summarized report is a success: {result:?}"
    );
    assert_eq!(
        result.output["report"],
        json!("Found call sites in a.rs:10 and b.rs:22; c.rs and d.rs remain unchecked."),
        "only the forced wrap-up's summary is the deliverable, not the mid-turn narration"
    );
    assert_eq!(
        result.output["capped"],
        json!(true),
        "a capped report must be flagged as partial: {result:?}"
    );
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "one-shot, spawn-and-wait: a capped exploration is still torn down as soon as its own \
         turn ends"
    );
}

/// The fallback shape: a cap halt with no report at all (the wrap-up never
/// ran -- a role that doesn't opt in -- or ran and produced nothing) stays
/// an ordinary error, with no follow-up affordance in the message (there is
/// none anymore -- follow-up was removed 2026-07-27).
#[test]
fn a_capped_exploration_without_a_report_is_still_an_error() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-capped-bare",
        json!({ "description": "map a call site", "prompt": "map the module" }),
    );
    let events = host.turn(0);
    events.send(user("map the module")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::HaltedByIterationCap))
        .unwrap();

    let result = requester.await_result();
    assert!(result.is_error, "{result:?}");
    assert!(
        result.output.get("capped").is_none(),
        "no capped marker without a report: {result:?}"
    );
    let message = result.output["message"].as_str().expect("a message");
    assert!(message.contains("ran out of turns"), "{message}");
    assert!(
        !message.contains("narrower"),
        "follow-up no longer exists, so the message must not suggest asking a narrower \
         question: {message}"
    );
    assert_eq!(*terminated.lock().unwrap(), vec![explore_session_id]);
}

/// The exploration session dying without ever ending its turn must resolve
/// the call, not hang it. Its last `Error` explains why, and there is
/// nothing left to follow up on.
#[test]
fn a_terminated_exploration_reports_its_last_error() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-dead",
        json!({ "description": "map a call site", "prompt": "map the module" }),
    );
    let events = host.turn(0);
    events.send(user("map the module")).unwrap();
    events
        .send(Event::Error(AgentError {
            message: "context window exceeded".to_string(),
        }))
        .unwrap();
    events
        .send(Event::StateChanged(SessionState::Terminated))
        .unwrap();

    let result = requester.await_result();
    assert!(result.is_error, "{result:?}");
    let message = result.output["message"].as_str().expect("a message");
    assert!(message.contains("terminated before finishing"), "{message}");
    assert!(message.contains("context window exceeded"), "{message}");
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![host.session_id],
        "a dead exploration is released immediately, not held until the turn ends"
    );
}

/// A session with no exploration host installed -- every test construction,
/// and an exploration session itself -- must get an actionable error result
/// rather than a silent no-op or a hung turn.
#[test]
fn a_session_without_an_exploration_host_fails_the_call_synchronously() {
    let requester = Requester::new(None);
    let execution = execute_call(
        &requester,
        "explore-nohost",
        json!({ "description": "map a call site", "prompt": "anything" }),
    );
    let output = finished_output(execution);
    assert_eq!(output["is_error"], json!(true));
    assert!(
        output["message"]
            .as_str()
            .expect("a message")
            .contains("not available"),
        "{output}"
    );
}

#[test]
fn an_empty_prompt_is_rejected_without_spawning_anything() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));
    let output = finished_output(execute_call(
        &requester,
        "explore-empty",
        json!({ "description": "map a call site", "prompt": "   " }),
    ));
    assert_eq!(output["is_error"], json!(true));
    assert!(host.started.lock().unwrap().is_empty());
    assert!(terminated.lock().unwrap().is_empty());
}

/// The 2026-07-27 two-field input shape: `description` is as required as
/// `prompt`, so the catalog schema's `required` list is enforced rather
/// than merely advertised. It is a label, though -- the exploration itself
/// is seeded with `prompt` alone (asserted below).
#[test]
fn a_missing_or_empty_description_is_rejected_without_spawning_anything() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    let missing = finished_output(execute_call(
        &requester,
        "explore-no-description",
        json!({ "prompt": "find the emit sites" }),
    ));
    assert_eq!(missing["is_error"], json!(true));
    assert!(
        missing["message"]
            .as_str()
            .expect("a message")
            .contains("`description`"),
        "{missing}"
    );

    let empty = finished_output(execute_call(
        &requester,
        "explore-empty-description",
        json!({ "description": "  ", "prompt": "find the emit sites" }),
    ));
    assert_eq!(empty["is_error"], json!(true));

    assert!(host.started.lock().unwrap().is_empty());
    assert!(terminated.lock().unwrap().is_empty());
}

#[test]
fn a_host_that_cannot_spawn_reports_why() {
    let host = ScriptedHost::failing("no writer configured");
    let requester = Requester::new(Some(host));
    let output = finished_output(execute_call(
        &requester,
        "explore-failed-spawn",
        json!({ "description": "map a call site", "prompt": "look" }),
    ));
    assert_eq!(output["is_error"], json!(true));
    assert!(
        output["message"]
            .as_str()
            .expect("a message")
            .contains("no writer configured"),
        "{output}"
    );
}

/// The prompt reaches the exploration session verbatim -- it is the only
/// context the exploration ever gets, `description` included: that field is
/// a display label for the requester's own transcript, never part of the
/// exploration's seeding.
#[test]
fn the_prompt_is_forwarded_to_the_exploration_session_verbatim() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));
    requester.request(
        "explore-prompt",
        json!({ "description": "map a call site", "prompt": "find the emit sites" }),
    );
    let events = host.turn(0);
    events.send(user("find the emit sites")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();
    assert_eq!(
        *host.started.lock().unwrap(),
        vec!["find the emit sites".to_string()]
    );
}

/// The simplified input surface (2026-07-27): a `session_id` field --
/// meaningful only under the old follow-up feature -- is no longer parsed
/// at all. A call that still sends one is not rejected; it is simply
/// ignored, and every call spawns a fresh exploration.
#[test]
fn a_stray_session_id_field_is_ignored_and_still_starts_fresh() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-1",
        json!({
            "description": "map a call site",
            "prompt": "find the emit sites",
            "session_id": SessionId::new().as_uuid().to_string(),
        }),
    );
    let events = host.turn(0);
    events.send(user("find the emit sites")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();

    assert_eq!(
        host.started.lock().unwrap().len(),
        1,
        "a stray session_id must not be treated as anything but noise"
    );
}

fn execute_call(requester: &Requester, call_id: &str, input: serde_json::Value) -> Execution {
    crate::tools::execution::execute_agent_tool(
        &NoHostTools,
        &requester.tool_state,
        requester.session_id,
        &ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: TOOL_ID.to_string(),
            input: input.into(),
        },
    )
}

fn finished_output(execution: Execution) -> serde_json::Value {
    let Execution::Auto(events) = execution else {
        panic!("expected a synchronously resolved error result, got {execution:?}");
    };
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output.0),
            _ => None,
        })
        .expect("a ToolCallFinished event")
}
