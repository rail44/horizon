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
/// Each `start`/`follow_up` mints a fresh channel and publishes its sending
/// half on `turns`, so a test can drive the second turn of a session it
/// already drove once -- the shape addendum B's follow-up needs.
struct ScriptedHost {
    session_id: SessionId,
    /// The sending half of every turn handed out so far, in order.
    turns: Mutex<Vec<Sender<Event>>>,
    started: Mutex<Vec<String>>,
    followed_up: Mutex<Vec<(SessionId, String)>>,
    terminated: Arc<Mutex<Vec<SessionId>>>,
    /// Every seed history `start` was handed, in order.
    seeds: Mutex<Vec<Vec<Event>>>,
    start_error: Option<String>,
    /// Sessions this host refuses to accept a follow-up for -- the stand-in
    /// for a session that ended between turns.
    dead: Mutex<Vec<SessionId>>,
}

impl ScriptedHost {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<SessionId>>>) {
        let terminated = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Self {
            session_id: SessionId::new(),
            turns: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            followed_up: Mutex::new(Vec::new()),
            terminated: terminated.clone(),
            seeds: Mutex::new(Vec::new()),
            start_error: None,
            dead: Mutex::new(Vec::new()),
        });
        (host, terminated)
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: SessionId::new(),
            turns: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            followed_up: Mutex::new(Vec::new()),
            terminated: Arc::new(Mutex::new(Vec::new())),
            seeds: Mutex::new(Vec::new()),
            start_error: Some(message.to_string()),
            dead: Mutex::new(Vec::new()),
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
    fn start(
        &self,
        prompt: String,
        seed_history: Vec<Event>,
    ) -> Result<StartedExploration, String> {
        if let Some(error) = &self.start_error {
            return Err(error.clone());
        }
        self.started.lock().unwrap().push(prompt);
        self.seeds.lock().unwrap().push(seed_history);
        Ok(StartedExploration {
            session_id: self.session_id,
            events: self.open_turn(),
        })
    }

    fn follow_up(
        &self,
        session_id: SessionId,
        prompt: String,
    ) -> Result<StartedExploration, String> {
        if self.dead.lock().unwrap().contains(&session_id) {
            return Err("that exploration session is no longer running".to_string());
        }
        self.followed_up.lock().unwrap().push((session_id, prompt));
        Ok(StartedExploration {
            session_id,
            events: self.open_turn(),
        })
    }

    fn terminate(&self, session_id: SessionId) {
        self.terminated.lock().unwrap().push(session_id);
        self.dead.lock().unwrap().push(session_id);
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
        Self::with_seed_mode(host, SeedMode::Fresh)
    }

    fn with_seed_mode(host: Option<Arc<dyn ExplorationHost>>, mode: SeedMode) -> Self {
        let session_id = SessionId::new();
        let tool_state = ToolSessionState::new(std::env::temp_dir())
            .with_exploration_host(host.clone())
            .with_exploration_seed_mode(mode);
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

    /// Runs one `agent.explore` call through the same entry point the
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

    /// Seeds this session's history directly, standing in for the turns the
    /// requester ran before it delegated anything.
    fn seed_history(&self, events: Vec<Event>) {
        self.live_state
            .extend_provider_events(events.into_iter().map(ProviderEvent::from));
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
/// exploration session is terminated once the requester's *turn* ends
/// (`docs/agent-explore-design.md` decisions 1 and 3, and addendum B's
/// lifetime rule).
#[test]
fn a_completed_exploration_returns_only_its_final_report() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    let call_id = requester.request(
        "explore-1",
        json!({ "prompt": "where are the WaitingForUser emissions?" }),
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

    assert!(
        terminated.lock().unwrap().is_empty(),
        "an exploration outlives the turn it answered, so a follow-up can reach it"
    );
    requester.provider_event(Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "the requester's turn end terminates it, exactly once"
    );
}

/// Addendum B: a second call naming the first call's `session_id` is a
/// follow-up -- the same exploration session answers again, no new one is
/// spawned, and the second turn's report is what comes back.
#[test]
fn a_follow_up_reuses_the_same_session_and_returns_its_second_report() {
    let (host, _terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request(
        "explore-1",
        json!({ "prompt": "where are the emit sites?" }),
    );
    let first = host.turn(0);
    first.send(user("where are the emit sites?")).unwrap();
    first.send(assistant("Only session.rs:1747.")).unwrap();
    first
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let first_result = requester.await_result();
    assert_eq!(
        first_result.output["report"],
        json!("Only session.rs:1747.")
    );

    // The requester noticed the report contradicts what it already measured
    // and repairs it on the session that still holds the files.
    let follow_up_id = requester.request(
        "explore-2",
        json!({
            "prompt": "my own trace shows a second site in frame.rs; check again",
            "session_id": explore_session_id.as_uuid().to_string(),
        }),
    );
    let second = host.turn(1);
    second
        .send(user(
            "my own trace shows a second site in frame.rs; check again",
        ))
        .unwrap();
    second
        .send(assistant("Correct -- session.rs:1747 and frame.rs:88."))
        .unwrap();
    second
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();

    let second_result = requester.await_result();
    assert_eq!(second_result.call_id, follow_up_id);
    assert!(!second_result.is_error, "{second_result:?}");
    assert_eq!(
        second_result.output["report"],
        json!("Correct -- session.rs:1747 and frame.rs:88.")
    );
    assert_eq!(
        second_result.output["session_id"],
        json!(explore_session_id.as_uuid().to_string()),
        "a follow-up stays on the same session"
    );
    assert_eq!(
        host.started.lock().unwrap().len(),
        1,
        "a follow-up must not spawn a second exploration session"
    );
    assert_eq!(
        *host.followed_up.lock().unwrap(),
        vec![(
            explore_session_id,
            "my own trace shows a second site in frame.rs; check again".to_string()
        )]
    );
}

/// Addendum B's failure shape: a session id this requester never started
/// (another session's, or one already terminated) is an ordinary error
/// result that names the fresh-spawn alternative -- never a hang, and never
/// a message delivered to a session that isn't an exploration of this
/// requester's.
#[test]
fn a_follow_up_to_an_unknown_session_errors_and_names_the_fresh_spawn_alternative() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    let output = finished_output(execute_call(
        &requester,
        "explore-stranger",
        json!({
            "prompt": "check again",
            "session_id": SessionId::new().as_uuid().to_string(),
        }),
    ));

    assert_eq!(output["is_error"], json!(true));
    let message = output["message"].as_str().expect("a message");
    assert!(message.contains("omit `session_id`"), "{message}");
    assert!(
        host.followed_up.lock().unwrap().is_empty(),
        "a stranger's session id must never reach the host"
    );
    assert!(host.started.lock().unwrap().is_empty());
}

/// The same, for an exploration that *was* this requester's but died with
/// the previous turn.
#[test]
fn a_follow_up_to_a_terminated_exploration_errors_cleanly() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-1", json!({ "prompt": "look around" }));
    let first = host.turn(0);
    first.send(user("look around")).unwrap();
    first.send(assistant("Found it.")).unwrap();
    first
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();
    requester.provider_event(Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(*terminated.lock().unwrap(), vec![explore_session_id]);

    let output = finished_output(execute_call(
        &requester,
        "explore-late",
        json!({
            "prompt": "one more thing",
            "session_id": explore_session_id.as_uuid().to_string(),
        }),
    ));

    assert_eq!(output["is_error"], json!(true));
    let message = output["message"].as_str().expect("a message");
    assert!(message.contains("omit `session_id`"), "{message}");
    assert!(
        host.followed_up.lock().unwrap().is_empty(),
        "a terminated session must not be asked anything"
    );
}

/// Addendum B's lifetime rule on the two non-cancellation paths: whatever
/// ended the requester's turn, no exploration survives into the next one.
#[test]
fn a_failed_requester_turn_terminates_its_exploration() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-1", json!({ "prompt": "look around" }));
    let events = host.turn(0);
    events.send(user("look around")).unwrap();
    events.send(assistant("Found it.")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();

    requester.provider_event(Event::TurnEnded(TurnEndReason::Failed));

    assert_eq!(*terminated.lock().unwrap(), vec![explore_session_id]);
}

/// The last path: the requesting session itself goes away, without any turn
/// end reaching `tools::processing` at all.
#[test]
fn the_requester_session_going_away_terminates_its_exploration() {
    let (host, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-1", json!({ "prompt": "look around" }));
    let events = host.turn(0);
    events.send(user("look around")).unwrap();
    events.send(assistant("Found it.")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();
    assert!(terminated.lock().unwrap().is_empty());

    // What `Requester::drop` does, and what `spawn_session_thread`'s cleanup
    // does in production.
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

    let call_id = requester.request("explore-cancel", json!({ "prompt": "keep looking" }));
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

    requester.request("explore-approval", json!({ "prompt": "look around" }));
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
        "a session parked on an approval can never end another turn, so it is torn down at once \
         rather than left for a follow-up to block on"
    );
    requester.provider_event(Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "and the turn end must not terminate it a second time"
    );
}

/// Decision 7: the turn cap (and every other non-completed turn end) comes
/// back as an error result carrying whatever report exists, so the
/// requester can recover by asking something narrower -- which, since
/// addendum B, it can do on this very session.
#[test]
fn a_capped_exploration_returns_its_partial_report_as_an_error_and_stays_followable() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-capped", json!({ "prompt": "map the module" }));
    let events = host.turn(0);
    events.send(user("map the module")).unwrap();
    events
        .send(assistant("Found two of the four sites."))
        .unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::HaltedByIterationCap))
        .unwrap();

    let result = requester.await_result();
    assert!(result.is_error, "{result:?}");
    assert_eq!(
        result.output["report"],
        json!("Found two of the four sites."),
        "a partial report is still worth returning"
    );
    assert!(
        result.output["message"]
            .as_str()
            .expect("a message")
            .contains("narrower"),
        "{result:?}"
    );
    assert!(
        terminated.lock().unwrap().is_empty(),
        "a capped exploration is back at WaitingForUser, so the narrower question can go to it"
    );
}

/// The exploration session dying without ever ending its turn must resolve
/// the call, not hang it. Its last `Error` explains why, and there is
/// nothing left to follow up on.
#[test]
fn a_terminated_exploration_reports_its_last_error() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-dead", json!({ "prompt": "map the module" }));
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
        json!({ "prompt": "anything" }),
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
        json!({ "prompt": "   " }),
    ));
    assert_eq!(output["is_error"], json!(true));
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
        json!({ "prompt": "look" }),
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
/// context the exploration ever gets.
#[test]
fn the_prompt_is_forwarded_to_the_exploration_session_verbatim() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));
    requester.request("explore-prompt", json!({ "prompt": "find the emit sites" }));
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

// --- addendum C: fork seeding -------------------------------------------

/// A requester's history as it looks at delegation time: some completed
/// work, then the mid-turn tail -- an asynchronous `bash` call still
/// running, and the `agent.explore` call being served. Neither has a result
/// yet, so neither may reach the seed.
fn requester_history(explore_call_id: &str) -> Vec<Event> {
    vec![
        assistant("Rig provider initialized."),
        user("fix the duplicate fs.edit"),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("read-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: json!({ "path": "src/edit.rs" }).into(),
        }),
        Event::ToolCallFinished(ToolCallResult::new(
            ToolCallId("read-1".to_string()),
            json!({ "text": "fn edit() {}" }),
        )),
        assistant("Two writes land for one edit; let me delegate the sweep."),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("bash-1".to_string()),
            tool_id: "bash".to_string(),
            input: json!({ "command": "cargo test" }).into(),
        }),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(explore_call_id.to_string()),
            tool_id: TOOL_ID.to_string(),
            input: json!({ "prompt": "where else does fs.edit write?" }).into(),
        }),
    ]
}

/// Under `fork`, the exploration inherits the requester's history -- minus
/// the unpaired tail, which no provider would accept.
#[test]
fn fork_seeding_hands_the_exploration_the_requesters_sanitized_history() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::with_seed_mode(Some(host.clone()), SeedMode::Fork);
    requester.seed_history(requester_history("explore-1"));

    requester.request(
        "explore-1",
        json!({ "prompt": "where else does fs.edit write?" }),
    );

    let seeds = host.seeds.lock().unwrap();
    let seed = seeds.first().expect("a seed history");
    assert!(
        seed.contains(&user("fix the duplicate fs.edit")),
        "the delegate must see the evidence the requester saw: {seed:?}"
    );
    assert!(
        seed.iter().any(|event| matches!(
            event,
            Event::ToolCallFinished(result) if result.call_id == ToolCallId("read-1".to_string())
        )),
        "a completed call keeps both halves: {seed:?}"
    );
    assert!(
        !seed.iter().any(|event| matches!(
            event,
            Event::ToolCallRequested(request)
                if request.tool_id == TOOL_ID || request.tool_id == "bash"
        )),
        "no call still awaiting a result may survive into the seed: {seed:?}"
    );
}

/// The default arm is byte-for-byte v1: nothing is copied at all.
#[test]
fn fresh_seeding_hands_the_exploration_nothing() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::with_seed_mode(Some(host.clone()), SeedMode::Fresh);
    requester.seed_history(requester_history("explore-1"));

    requester.request(
        "explore-1",
        json!({ "prompt": "where else does fs.edit write?" }),
    );

    assert_eq!(*host.seeds.lock().unwrap(), vec![Vec::<Event>::new()]);
}

/// Seeding never fails a call: a requester with nothing to copy degrades to
/// a fresh spawn and says so, rather than hanging or erroring.
#[test]
fn fork_seeding_with_no_history_degrades_to_a_fresh_spawn_with_a_note() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::with_seed_mode(Some(host.clone()), SeedMode::Fork);

    requester.request("explore-1", json!({ "prompt": "look around" }));
    let events = host.turn(0);
    events.send(user("look around")).unwrap();
    events.send(assistant("Nothing found.")).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();

    let result = requester.await_result();
    assert!(!result.is_error, "{result:?}");
    assert!(
        result.output["note"]
            .as_str()
            .expect("a note")
            .contains("started fresh"),
        "{result:?}"
    );
    assert_eq!(*host.seeds.lock().unwrap(), vec![Vec::<Event>::new()]);
}

/// A follow-up continues a session that already has whatever history it was
/// started with, so it never re-seeds -- not even under `fork`.
#[test]
fn a_follow_up_does_not_re_seed() {
    let (host, _terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::with_seed_mode(Some(host.clone()), SeedMode::Fork);
    requester.seed_history(requester_history("explore-1"));

    requester.request("explore-1", json!({ "prompt": "where does it write?" }));
    let first = host.turn(0);
    first.send(user("where does it write?")).unwrap();
    first.send(assistant("Only edit.rs.")).unwrap();
    first
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    let _ = requester.await_result();

    requester.request(
        "explore-2",
        json!({
            "prompt": "check write.rs too",
            "session_id": explore_session_id.as_uuid().to_string(),
        }),
    );

    assert_eq!(
        host.seeds.lock().unwrap().len(),
        1,
        "only the fresh spawn is seeded"
    );
}

/// The sanitizer's postcondition, stated directly: in the output every tool
/// call has exactly one result and every result has exactly one call, in
/// that order -- which is what "provider-valid" reduces to here.
#[test]
fn sanitizing_pairs_every_surviving_tool_call_with_a_result() {
    let requested = |id: &str| {
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(id.to_string()),
            tool_id: "fs.read".to_string(),
            input: json!({}).into(),
        })
    };
    let finished = |id: &str| {
        Event::ToolCallFinished(ToolCallResult::new(ToolCallId(id.to_string()), json!({})))
    };

    let sanitized = sanitize_seed_history(&[
        // An orphan result with no call before it.
        finished("ghost"),
        requested("paired"),
        finished("paired"),
        // A call retried before its first attempt resolved.
        requested("retried"),
        requested("retried"),
        finished("retried"),
        assistant("delegating"),
        // The in-flight tail.
        requested("in-flight"),
    ]);

    let mut open: Vec<ToolCallId> = Vec::new();
    let mut pairs = 0;
    for event in &sanitized {
        match event {
            Event::ToolCallRequested(request) => {
                assert!(
                    !open.contains(&request.call_id),
                    "a second unresolved call for the same id survived: {sanitized:?}"
                );
                open.push(request.call_id.clone());
            }
            Event::ToolCallFinished(result) => {
                let position = open
                    .iter()
                    .position(|call_id| call_id == &result.call_id)
                    .unwrap_or_else(|| panic!("orphan result survived: {sanitized:?}"));
                open.remove(position);
                pairs += 1;
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "an unpaired call survived: {sanitized:?}");
    assert_eq!(pairs, 2, "both genuinely completed calls survive");
    assert!(
        sanitized.contains(&assistant("delegating")),
        "non-tool events pass through untouched: {sanitized:?}"
    );
}

/// A history with nothing outstanding is passed through unchanged -- the
/// sanitizer is a tail fixup, not a filter on ordinary history.
#[test]
fn sanitizing_a_settled_history_changes_nothing() {
    let history = vec![
        user("do the thing"),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("read-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: json!({}).into(),
        }),
        Event::ToolCallFinished(ToolCallResult::new(
            ToolCallId("read-1".to_string()),
            json!({}),
        )),
        assistant("done"),
    ];

    assert_eq!(sanitize_seed_history(&history), history);
}

#[test]
fn the_seed_mode_switch_parses_exactly_fresh_and_fork() {
    assert_eq!(SeedMode::parse("fork"), Some(SeedMode::Fork));
    assert_eq!(SeedMode::parse("  FORK\n"), Some(SeedMode::Fork));
    assert_eq!(SeedMode::parse("fresh"), Some(SeedMode::Fresh));
    assert_eq!(SeedMode::parse(""), Some(SeedMode::Fresh));
    assert_eq!(SeedMode::parse("1"), None);
    assert_eq!(SeedMode::parse("copy"), None);
    assert_eq!(
        SeedMode::default(),
        SeedMode::Fresh,
        "fork is opt-in: an unset switch must never change a session's behavior"
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
