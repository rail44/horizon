use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;
use serde_json::json;

use super::notify::INLINE_REPORT_CAP_CHARS;
use super::*;
use crate::contract::{Error as AgentError, Message, ProviderEvent, ToolCallId};
use crate::live::LiveState;
use crate::tools::execution::HostTools;
use crate::tools::processing::process_agent_provider_event;
use crate::tools::state::{register_session_runtime, unregister_session_runtime};

/// How long a test waits for a waiter thread to fold a scripted event
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

/// A scripted stand-in for `horizon-sessiond`'s real task host: it hands
/// the tool one half of a channel the test drives event by event, and
/// records every `terminate` call so the teardown requirements are directly
/// assertable. Deterministic and offline -- no provider, no session thread,
/// nothing to wait on but the waiter's own fold.
///
/// Each `start` mints a fresh child session id and a fresh channel, so a
/// test can run several children at once (the concurrency cap needs
/// exactly that).
struct ScriptedHost {
    children: Mutex<Vec<(SessionId, Sender<Event>)>>,
    started: Mutex<Vec<String>>,
    terminated: Arc<Mutex<Vec<SessionId>>>,
    start_error: Option<String>,
}

impl ScriptedHost {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<SessionId>>>) {
        let terminated = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Self {
            children: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            terminated: terminated.clone(),
            start_error: None,
        });
        (host, terminated)
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            children: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            terminated: Arc::new(Mutex::new(Vec::new())),
            start_error: Some(message.to_string()),
        })
    }

    /// The `index`-th child this host handed out: its session id and the
    /// sending half of its event stream.
    fn child(&self, index: usize) -> (SessionId, Sender<Event>) {
        self.children
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("child {index} was never started"))
    }
}

impl ExplorationHost for ScriptedHost {
    fn start(&self, prompt: String) -> Result<StartedExploration, String> {
        if let Some(error) = &self.start_error {
            return Err(error.clone());
        }
        self.started.lock().unwrap().push(prompt);
        let session_id = SessionId::new();
        let (tx, events) = crossbeam_channel::unbounded();
        self.children.lock().unwrap().push((session_id, tx));
        Ok(StartedExploration { session_id, events })
    }

    fn terminate(&self, session_id: SessionId) {
        self.terminated.lock().unwrap().push(session_id);
    }
}

/// A requester session wired exactly as `horizon-sessiond`'s `run_session`
/// wires one.
struct Requester {
    session_id: SessionId,
    tool_state: ToolSessionState,
    live_state: LiveState,
}

impl Requester {
    fn new(host: Option<Arc<dyn ExplorationHost>>) -> Self {
        let session_id = SessionId::new();
        let tool_state = ToolSessionState::new(std::env::temp_dir()).with_exploration_host(host);
        let live_state = LiveState::with_disabled_persistence();
        let (results_tx, _results) = crossbeam_channel::unbounded();
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
        }
    }

    /// Runs one tool call through the same entry point the session loop
    /// uses, folding whatever it produces into this session's live state
    /// (as `horizon-sessiond`'s `handle_provider_event` does), and returns
    /// the call's own result output.
    fn call(&self, call_id: &str, tool_id: &str, input: serde_json::Value) -> serde_json::Value {
        let processing = process_agent_provider_event(
            &NoHostTools,
            &self.tool_state,
            self.session_id,
            ProviderEvent::from(Event::ToolCallRequested(ToolCallRequest {
                call_id: ToolCallId(call_id.to_string()),
                tool_id: tool_id.to_string(),
                input: input.into(),
            })),
        );
        let output = processing
            .horizon_events
            .iter()
            .find_map(|event| match &event.event {
                Event::ToolCallFinished(result) => Some(result.output.0.clone()),
                _ => None,
            })
            .expect("every task/task_output call resolves synchronously");
        self.live_state
            .extend_provider_events(processing.horizon_events);
        output
    }

    fn launch(&self, call_id: &str, description: &str, prompt: &str) -> serde_json::Value {
        self.call(
            call_id,
            TOOL_ID,
            json!({ "description": description, "prompt": prompt }),
        )
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

    /// Blocks until this session has a notification queued, or `WAIT`
    /// elapses -- the waiter thread queues asynchronously.
    fn await_notification(&self) -> String {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(text) = take_notification(self.session_id) {
                return text;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no task notification was queued within {WAIT:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
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

/// Drives a scripted child through a whole successful run.
fn complete_child(events: &Sender<Event>, prompt: &str, report: &str) {
    events.send(user(prompt)).unwrap();
    events.send(assistant(report)).unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
}

fn wait_until(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + WAIT;
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Decision 1: the launch is non-blocking. The call resolves right now with
/// `{session_id, description, status: "started"}` -- there is no
/// `Execution::Started`, and therefore no pending call for the requester's
/// turn to wait on.
#[test]
fn a_launch_returns_immediately_with_a_started_receipt() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    let output = requester.launch("t1", "map the emit sites", "where are they?");

    assert_eq!(output["status"], json!("started"));
    assert_eq!(output["description"], json!("map the emit sites"));
    let (child_id, _events) = host.child(0);
    assert_eq!(
        output["session_id"],
        json!(child_id.as_uuid().to_string()),
        "the task session id is the handle `task_output` takes"
    );
    assert!(output.get("is_error").is_none(), "{output}");
    assert_eq!(
        *host.started.lock().unwrap(),
        vec!["where are they?".to_string()],
        "the prompt reaches the task session verbatim, and `description` is not part of it"
    );
    assert!(
        take_notification(requester.session_id).is_none(),
        "nothing is delivered until the child actually finishes"
    );
}

/// Decision 2's coalescing rule: several children finishing between two
/// provider rounds leave the queue together, as exactly one notification
/// block naming each of them.
#[test]
fn several_completions_coalesce_into_one_notification() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map the emit sites", "where are they?");
    requester.launch("t2", "list the consumers", "who reads them?");
    let (_, first) = host.child(0);
    let (_, second) = host.child(1);
    complete_child(&first, "where are they?", "Emitted at session.rs:1747.");
    complete_child(&second, "who reads them?", "Consumed at frame.rs:88.");

    // The two waiter threads queue independently; wait until both landed
    // before draining, which is what "between two rounds" means.
    wait_until(
        || super::children::pending_count(requester.session_id) >= 2,
        "both completions to be queued",
    );
    let text = requester.await_notification();

    assert_eq!(
        text.matches("session_id:").count(),
        2,
        "both children must be named in the one block: {text}"
    );
    assert!(text.contains("map the emit sites"), "{text}");
    assert!(text.contains("list the consumers"), "{text}");
    assert!(text.contains("Emitted at session.rs:1747."), "{text}");
    assert!(text.contains("Consumed at frame.rs:88."), "{text}");
    assert!(
        text.contains("system notification, not a message from the user"),
        "the notification must say plainly that the user did not write it: {text}"
    );
    assert!(
        take_notification(requester.session_id).is_none(),
        "a drain empties the queue"
    );
}

/// A report longer than the inline budget is cut, and the cut names the
/// exact `task_output` call that fetches the rest (decision 2/3).
#[test]
fn a_long_report_is_truncated_with_a_pointer_to_task_output() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map everything", "map it");
    let (child_id, events) = host.child(0);
    let report = "x".repeat(INLINE_REPORT_CAP_CHARS + 500);
    complete_child(&events, "map it", &report);

    let text = requester.await_notification();
    assert!(
        text.contains("report truncated"),
        "an over-budget report must say it was cut: {text}"
    );
    assert!(
        text.contains(&format!(
            "call task_output with session_id \"{}\"",
            child_id.as_uuid()
        )),
        "the cut must name the exact fetch: {text}"
    );
    assert!(
        !text.contains(&"x".repeat(INLINE_REPORT_CAP_CHARS + 1)),
        "no more than the inline budget may be inlined"
    );

    // ...and the full text is still there to fetch.
    let fetched = requester.call(
        "o1",
        OUTPUT_TOOL_ID,
        json!({ "session_id": child_id.as_uuid().to_string() }),
    );
    assert_eq!(fetched["report"], json!(report));
}

/// Decision 5: a fourth concurrent launch fails fast, and the refusal names
/// every running child (id and description) so the model can decide what to
/// wait for instead of blindly retrying.
#[test]
fn a_fourth_concurrent_launch_is_refused_and_names_the_running_tasks() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    for index in 0..super::children::MAX_CONCURRENT_TASKS {
        let output = requester.launch(
            &format!("t{index}"),
            &format!("task number {index}"),
            "look around",
        );
        assert_eq!(output["status"], json!("started"), "{output}");
    }

    let refused = requester.launch("t-over", "one too many", "look around");
    assert_eq!(refused["is_error"], json!(true), "{refused}");
    let message = refused["message"].as_str().expect("a message");
    for index in 0..super::children::MAX_CONCURRENT_TASKS {
        let (child_id, _) = host.child(index);
        assert!(
            message.contains(&child_id.as_uuid().to_string()),
            "the refusal must name running task {index}: {message}"
        );
        assert!(
            message.contains(&format!("task number {index}")),
            "the refusal must describe running task {index}: {message}"
        );
    }
    assert_eq!(
        host.started.lock().unwrap().len(),
        super::children::MAX_CONCURRENT_TASKS,
        "the refused launch must not have spawned anything"
    );

    // One finishing frees a slot.
    let (_, first) = host.child(0);
    complete_child(&first, "look around", "done");
    wait_until(
        || {
            requester.launch("t-retry", "after a slot freed", "look around")["status"]
                == json!("started")
        },
        "a freed slot to admit a new launch",
    );
}

/// Decision 3, all three paths of `task_output`: an unknown id errors, an
/// in-flight child reports as still running (not an error -- there is
/// nothing wrong), and a finished one returns the full stored report.
#[test]
fn task_output_reports_unknown_running_and_finished_tasks() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    let unknown = requester.call(
        "o0",
        OUTPUT_TOOL_ID,
        json!({ "session_id": SessionId::new().as_uuid().to_string() }),
    );
    assert_eq!(unknown["is_error"], json!(true), "{unknown}");
    assert!(
        unknown["message"]
            .as_str()
            .expect("a message")
            .contains("was launched from this session"),
        "{unknown}"
    );

    requester.launch("t1", "map the emit sites", "where are they?");
    let (child_id, events) = host.child(0);

    let running = requester.call(
        "o1",
        OUTPUT_TOOL_ID,
        json!({ "session_id": child_id.as_uuid().to_string() }),
    );
    assert!(running.get("is_error").is_none(), "{running}");
    assert_eq!(running["status"], json!("running"));
    assert_eq!(running["description"], json!("map the emit sites"));

    complete_child(&events, "where are they?", "Emitted at session.rs:1747.");
    wait_until(
        || {
            requester.call(
                "o2",
                OUTPUT_TOOL_ID,
                json!({ "session_id": child_id.as_uuid().to_string() }),
            )["status"]
                == json!("finished")
        },
        "the child's report to become fetchable",
    );
    let finished = requester.call(
        "o3",
        OUTPUT_TOOL_ID,
        json!({ "session_id": child_id.as_uuid().to_string() }),
    );
    assert_eq!(finished["report"], json!("Emitted at session.rs:1747."));
    assert_eq!(finished["description"], json!("map the emit sites"));
    assert!(finished.get("is_error").is_none(), "{finished}");
}

/// The ownership check (decision 3): another session's task id is
/// indistinguishable from an unknown one -- a session must not be able to
/// probe what other sessions are running.
#[test]
fn task_output_refuses_a_task_owned_by_another_session() {
    let (host, _terminated) = ScriptedHost::new();
    let owner = Requester::new(Some(host.clone()));
    let stranger = Requester::new(Some(host.clone()));

    owner.launch("t1", "map the emit sites", "where are they?");
    let (child_id, _events) = host.child(0);

    let refused = stranger.call(
        "o1",
        OUTPUT_TOOL_ID,
        json!({ "session_id": child_id.as_uuid().to_string() }),
    );
    assert_eq!(refused["is_error"], json!(true), "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .expect("a message")
            .contains("was launched from this session"),
        "an unowned id must read exactly like an unknown one: {refused}"
    );
}

/// Decision 4: children are session-scoped, not turn-scoped. Cancelling the
/// requester's turn must leave them running, and a result that lands
/// afterwards stays queued for whatever turn comes next.
#[test]
fn cancelling_the_requesters_turn_leaves_children_running_and_results_queued() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map the emit sites", "where are they?");
    let (child_id, events) = host.child(0);

    // What a cancelled turn produces: a synthetic result for every call the
    // provider still had outstanding, then the turn's own end.
    requester.provider_event(Event::ToolCallFinished(
        crate::tools::cancelled_tool_call_result(ToolCallId("some-other-call".to_string())),
    ));
    requester.provider_event(Event::TurnEnded(TurnEndReason::Cancelled));

    assert!(
        terminated.lock().unwrap().is_empty(),
        "a cancelled turn must not vaporize in-flight investigation"
    );

    complete_child(&events, "where are they?", "Emitted at session.rs:1747.");
    let text = requester.await_notification();
    assert!(
        text.contains("Emitted at session.rs:1747."),
        "an undelivered completion survives cancellation and delivers on the next turn: {text}"
    );
    assert_eq!(*terminated.lock().unwrap(), vec![child_id]);
}

/// Decision 4's other half: the requesting session going away terminates
/// every child it launched, and drops whatever it never collected.
#[test]
fn the_requester_session_going_away_terminates_its_children() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));
    let requester_id = requester.session_id;

    requester.launch("t1", "still running", "look around");
    requester.launch("t2", "also running", "look around");
    let (first, _) = host.child(0);
    let (second, _) = host.child(1);

    unregister_session_runtime(requester_id);

    let mut killed = terminated.lock().unwrap().clone();
    killed.sort_by_key(|id| id.as_uuid());
    let mut expected = vec![first, second];
    expected.sort_by_key(|id| id.as_uuid());
    assert_eq!(killed, expected);
    assert!(take_notification(requester_id).is_none());
}

/// A child that ends its turn without a report, dies, or parks on an
/// approval still resolves into a notification rather than disappearing:
/// the requester must always learn what happened to a task it launched.
#[test]
fn a_failed_child_is_reported_as_a_failure_notification() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map the module", "map it");
    let (child_id, events) = host.child(0);
    events.send(user("map it")).unwrap();
    events
        .send(Event::Error(AgentError {
            message: "context window exceeded".to_string(),
        }))
        .unwrap();
    events
        .send(Event::StateChanged(SessionState::Terminated))
        .unwrap();

    let text = requester.await_notification();
    assert!(text.contains("map the module"), "{text}");
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("context window exceeded"), "{text}");
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![child_id],
        "a dead child is released immediately"
    );
}

/// Decision 4 of `docs/agent-explore-design.md`: approvals are unreachable
/// for a v1 child by construction, but if one ever surfaces the task fails
/// at once rather than waiting forever on a human who is not watching that
/// session.
#[test]
fn an_approval_in_a_child_fails_it_immediately() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map the module", "look around");
    let (child_id, events) = host.child(0);
    events.send(user("look around")).unwrap();
    events
        .send(Event::StateChanged(SessionState::WaitingForApproval))
        .unwrap();

    let text = requester.await_notification();
    assert!(text.contains("approval"), "{text}");
    assert_eq!(*terminated.lock().unwrap(), vec![child_id]);
}

/// The 2026-07-27 addendum's partial success, unchanged by the
/// asynchronous cutover: a capped child that produced a forced wrap-up
/// summary is reported as completed-but-partial, not as a failure.
#[test]
fn a_capped_child_is_reported_as_a_partial_success() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.launch("t1", "map the module", "map it");
    let (child_id, events) = host.child(0);
    events.send(user("map it")).unwrap();
    events
        .send(assistant("Found a.rs:10; c.rs remains unchecked."))
        .unwrap();
    events
        .send(Event::TurnEnded(TurnEndReason::HaltedByIterationCap))
        .unwrap();

    let text = requester.await_notification();
    assert!(text.contains("turn limit"), "{text}");
    assert!(text.contains("Found a.rs:10"), "{text}");

    let fetched = requester.call(
        "o1",
        OUTPUT_TOOL_ID,
        json!({ "session_id": child_id.as_uuid().to_string() }),
    );
    assert_eq!(fetched["capped"], json!(true));
    assert!(fetched.get("is_error").is_none(), "{fetched}");
}

/// A session with no task host installed -- every test construction, and a
/// task child itself -- must get an actionable error result rather than a
/// silent no-op or a hung turn.
#[test]
fn a_session_without_a_task_host_fails_the_call_synchronously() {
    let requester = Requester::new(None);
    let output = requester.launch("t1", "map a call site", "anything");
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
fn an_empty_prompt_or_description_is_rejected_without_spawning_anything() {
    let (host, terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    for input in [
        json!({ "description": "map a call site", "prompt": "   " }),
        json!({ "prompt": "find the emit sites" }),
        json!({ "description": "  ", "prompt": "find the emit sites" }),
    ] {
        let output = requester.call("t1", TOOL_ID, input.clone());
        assert_eq!(output["is_error"], json!(true), "{input} -> {output}");
    }

    assert!(host.started.lock().unwrap().is_empty());
    assert!(terminated.lock().unwrap().is_empty());
}

#[test]
fn a_host_that_cannot_spawn_reports_why() {
    let host = ScriptedHost::failing("no writer configured");
    let requester = Requester::new(Some(host));
    let output = requester.launch("t1", "map a call site", "look");
    assert_eq!(output["is_error"], json!(true));
    assert!(
        output["message"]
            .as_str()
            .expect("a message")
            .contains("no writer configured"),
        "{output}"
    );
}

#[test]
fn task_output_rejects_a_malformed_session_id() {
    let (host, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host));

    let missing = requester.call("o1", OUTPUT_TOOL_ID, json!({}));
    assert_eq!(missing["is_error"], json!(true), "{missing}");

    let malformed = requester.call("o2", OUTPUT_TOOL_ID, json!({ "session_id": "not-a-uuid" }));
    assert_eq!(malformed["is_error"], json!(true), "{malformed}");
    assert!(
        malformed["message"]
            .as_str()
            .expect("a message")
            .contains("not a task session id"),
        "{malformed}"
    );
}
