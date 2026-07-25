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
/// records every `terminate` call so the cancellation and auto-teardown
/// requirements are directly assertable. Deterministic and offline -- no
/// provider, no session thread, nothing to wait on but the tool's own fold.
struct ScriptedHost {
    session_id: SessionId,
    events: Mutex<Option<Receiver<Event>>>,
    started: Mutex<Vec<String>>,
    terminated: Arc<Mutex<Vec<SessionId>>>,
    start_error: Option<String>,
}

impl ScriptedHost {
    fn new() -> (Arc<Self>, Sender<Event>, Arc<Mutex<Vec<SessionId>>>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let terminated = Arc::new(Mutex::new(Vec::new()));
        let host = Arc::new(Self {
            session_id: SessionId::new(),
            events: Mutex::new(Some(rx)),
            started: Mutex::new(Vec::new()),
            terminated: terminated.clone(),
            start_error: None,
        });
        (host, tx, terminated)
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: SessionId::new(),
            events: Mutex::new(None),
            started: Mutex::new(Vec::new()),
            terminated: Arc::new(Mutex::new(Vec::new())),
            start_error: Some(message.to_string()),
        })
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
            events: self
                .events
                .lock()
                .unwrap()
                .take()
                .expect("the scripted host starts at most one exploration"),
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
        let tool_state =
            ToolSessionState::new(std::env::temp_dir()).with_exploration_host(host.clone());
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
/// exploration session is terminated once its report has landed
/// (`docs/agent-explore-design.md` decisions 1 and 3, and the second test
/// requirement).
#[test]
fn a_completed_exploration_returns_only_its_final_report_and_is_terminated() {
    let (host, events, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    let call_id = requester.request(
        "explore-1",
        json!({ "prompt": "where are the WaitingForUser emissions?" }),
    );

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

    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "a finished exploration session must be terminated exactly once"
    );
}

/// Decision 6: cancelling the requester's turn terminates the exploration
/// session. The provider's synthetic `ToolCallFinished` for the pending
/// call is the cancellation signal, exactly as it is for `bash`/`web`.
#[test]
fn cancelling_the_requesters_turn_terminates_the_exploration_session() {
    let (host, events, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    let call_id = requester.request("explore-cancel", json!({ "prompt": "keep looking" }));
    events.send(user("keep looking")).unwrap();
    events.send(assistant("still searching...")).unwrap();

    // What a cancelled turn produces for a still-pending tool call.
    let processing = process_agent_provider_event(
        &NoHostTools,
        &requester.tool_state,
        requester.session_id,
        ProviderEvent::from(Event::ToolCallFinished(
            crate::tools::cancelled_tool_call_result(call_id.clone()),
        )),
    );
    requester
        .live_state
        .extend_provider_events(processing.horizon_events);

    let deadline = std::time::Instant::now() + WAIT;
    while terminated.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        *terminated.lock().unwrap(),
        vec![explore_session_id],
        "cancelling the requester's turn must terminate the exploration session"
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
    let (host, events, terminated) = ScriptedHost::new();
    let explore_session_id = host.session_id;
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-approval", json!({ "prompt": "look around" }));
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
        "the exploration session must still be torn down"
    );
}

/// Decision 7: the turn cap (and every other non-completed turn end) comes
/// back as an error result carrying whatever report exists, so the
/// requester can recover by asking something narrower.
#[test]
fn a_capped_exploration_returns_its_partial_report_as_an_error() {
    let (host, events, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-capped", json!({ "prompt": "map the module" }));
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
}

/// The exploration session dying without ever ending its turn must resolve
/// the call, not hang it. Its last `Error` explains why.
#[test]
fn a_terminated_exploration_reports_its_last_error() {
    let (host, events, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));

    requester.request("explore-dead", json!({ "prompt": "map the module" }));
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
    let (host, _events, terminated) = ScriptedHost::new();
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
    let (host, events, _terminated) = ScriptedHost::new();
    let requester = Requester::new(Some(host.clone()));
    requester.request("explore-prompt", json!({ "prompt": "find the emit sites" }));
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
