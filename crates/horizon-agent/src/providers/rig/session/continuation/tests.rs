use super::*;
use crate::contract::{ConversationRecord, ProviderEvent, TurnEndReason};
use crate::decision::{DecisionConfig, DecisionResponse, Usage};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One scripted answer of the mock decision client.
enum Scripted {
    Choice(&'static str, f64),
    Error,
    Hang,
}

/// Records every request and answers from a queue; an exhausted queue
/// repeats `fallback`.
struct MockDecision {
    requests: Mutex<Vec<DecisionRequest>>,
    queue: Mutex<VecDeque<Scripted>>,
    repeat: Option<(&'static str, f64)>,
}

impl MockDecision {
    fn new(queue: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            queue: Mutex::new(queue.into()),
            repeat: None,
        })
    }

    fn always(choice: &'static str, probability: f64) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            queue: Mutex::new(VecDeque::new()),
            repeat: Some((choice, probability)),
        })
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl DecisionClient for MockDecision {
    async fn decide(&self, request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
        self.requests.lock().unwrap().push(request);
        let next = self.queue.lock().unwrap().pop_front();
        let (choice, probability) = match next {
            Some(Scripted::Choice(choice, probability)) => (choice, probability),
            Some(Scripted::Error) => anyhow::bail!("decision endpoint said no"),
            Some(Scripted::Hang) => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            None => self.repeat.expect("unscripted decision call"),
        };
        let other = if choice == MODEL { USER } else { MODEL };
        Ok(DecisionResponse {
            model: "clef".into(),
            answers: BTreeMap::from([(
                QUESTION.to_string(),
                Answer::Choice {
                    choice: choice.to_string(),
                    probabilities: BTreeMap::from([
                        (choice.to_string(), probability),
                        (other.to_string(), 1.0 - probability),
                    ]),
                    confidence: 0.5,
                },
            )]),
            usage: Usage::default(),
        })
    }
}

fn check(client: Arc<MockDecision>) -> ContinuationCheck {
    ContinuationCheck {
        client,
        model: "clef:cloud".into(),
        threshold: DEFAULT_CONTINUATION_THRESHOLD,
        timeout: Duration::from_millis(200),
    }
}

/// Serves one streamed chat completion per connection, answering with the
/// next of `replies`, and returns every request body it read.
fn serve_replies(
    listener: TcpListener,
    replies: Vec<&'static str>,
) -> tokio::task::JoinHandle<Vec<Value>> {
    tokio::spawn(async move {
        let mut bodies = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (body_start, body_len) = loop {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "request ended before its headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .expect("JSON request content length");
                    break (end + 4, length);
                }
            };
            let received = request.len();
            request.resize(body_start + body_len, 0);
            if received < request.len() {
                socket.read_exact(&mut request[received..]).await.unwrap();
            }
            bodies.push(serde_json::from_slice(&request[body_start..]).unwrap());
            let chunk = json!({
                "id": "reply", "object": "chat.completion.chunk", "created": 0,
                "model": "chat-model",
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": reply},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
            });
            let response = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        bodies
    })
}

/// A session on a local fake provider with `check` installed.
async fn session(
    check: Option<ContinuationCheck>,
    replies: Vec<&'static str>,
) -> (
    SessionLoopState,
    crossbeam_channel::Receiver<ProviderEvent>,
    tokio::task::JoinHandle<Vec<Value>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let key_var = "HORIZON_TEST_CONTINUATION_KEY";
    std::env::set_var(key_var, "local-test-key");
    let (events_tx, events) = crossbeam_channel::unbounded();
    let state = SessionLoopState {
        config: RigAgentConfig {
            api_key_present: true,
            api_key_env: key_var.into(),
            base_url: Some(base_url),
            model: "chat-model".into(),
            ..Default::default()
        },
        continuation: ContinuationState {
            check,
            ..Default::default()
        },
        events_tx,
        ..Default::default()
    };
    (state, events, serve_replies(listener, replies))
}

fn drain(events: &crossbeam_channel::Receiver<ProviderEvent>) -> Vec<Event> {
    events
        .try_iter()
        .filter_map(ProviderEvent::into_event)
        .collect()
}

fn verdicts(events: &[Event]) -> Vec<ContinuationVerdict> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ContinuationVerdict(verdict) => Some(verdict.clone()),
            _ => None,
        })
        .collect()
}

fn continuations(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
                Event::MessageCommitted(AgentMessage { role: MessageRole::AutoContinue, text })
                    if text == CONTINUATION_PROMPT)
        })
        .count()
}

fn turn_ends(events: &[Event]) -> Vec<TurnEndReason> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::TurnEnded(reason) => Some(*reason),
            _ => None,
        })
        .collect()
}

/// The last user-role text a provider request carried.
fn last_user_text(body: &Value) -> String {
    let message = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .expect("a user message");
    match &message["content"] {
        Value::String(text) => text.clone(),
        content => content
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
    }
}

#[tokio::test]
async fn a_confident_model_answer_continues_the_turn_once_without_ending_it() {
    let client = MockDecision::new(vec![
        Scripted::Choice(MODEL, 0.9),
        Scripted::Choice(USER, 0.9),
    ]);
    let (mut state, events, server) = session(
        Some(check(client.clone())),
        vec!["I will now run the tests.", "Tests pass; done."],
    )
    .await;
    state.handle_user_message("fix the bug".into()).await;
    let emitted = drain(&events);
    let bodies = server.await.unwrap();

    assert_eq!(continuations(&emitted), 1);
    assert_eq!(bodies.len(), 2);
    assert_eq!(last_user_text(&bodies[1]), CONTINUATION_PROMPT);
    assert!(emitted.iter().any(|event| matches!(event,
        Event::ConversationRecorded(ConversationRecord::Input {
            kind: ConversationInputKind::Continuation,
            text,
        }) if text == CONTINUATION_PROMPT)));
    assert_eq!(turn_ends(&emitted), vec![TurnEndReason::Completed]);
    let verdicts = verdicts(&emitted);
    assert_eq!(verdicts.len(), 2);
    assert!(verdicts[0].continued);
    assert_eq!(verdicts[0].next_speaker.as_deref(), Some(MODEL));
    assert_eq!(verdicts[0].model_probability, Some(0.9));
    assert_eq!(verdicts[0].continuations_so_far, 0);
    assert!(!verdicts[1].continued);
    assert_eq!(verdicts[1].continuations_so_far, 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].state["user_request"], "fix the bug");
    assert_eq!(requests[1].state["user_request"], "fix the bug");
    assert_eq!(requests[1].state["assistant_reply"], "Tests pass; done.");
}

#[tokio::test]
async fn a_user_answer_hands_back() {
    let client = MockDecision::new(vec![Scripted::Choice(USER, 0.9)]);
    let (mut state, events, server) =
        session(Some(check(client.clone())), vec!["Which branch?"]).await;
    state.handle_user_message("merge it".into()).await;
    let emitted = drain(&events);
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(continuations(&emitted), 0);
    let verdicts = verdicts(&emitted);
    assert_eq!(verdicts.len(), 1);
    assert!(!verdicts[0].continued);
    assert_eq!(verdicts[0].next_speaker.as_deref(), Some(USER));
    assert_eq!(verdicts[0].fallback_reason, None);
    assert_eq!(turn_ends(&emitted), vec![TurnEndReason::Completed]);
}

#[tokio::test]
async fn a_model_answer_below_the_threshold_hands_back() {
    let client = MockDecision::new(vec![Scripted::Choice(MODEL, 0.7)]);
    let (mut state, events, server) =
        session(Some(check(client.clone())), vec!["Next I could refactor."]).await;
    state.handle_user_message("look at this".into()).await;
    let emitted = drain(&events);
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(continuations(&emitted), 0);
    let verdicts = verdicts(&emitted);
    assert_eq!(verdicts.len(), 1);
    assert!(!verdicts[0].continued);
    assert_eq!(verdicts[0].next_speaker.as_deref(), Some(MODEL));
    assert_eq!(verdicts[0].threshold, 0.8);
}

#[tokio::test]
async fn continuations_stop_at_the_per_turn_cap() {
    let client = MockDecision::always(MODEL, 0.95);
    let (mut state, events, server) = session(
        Some(check(client.clone())),
        vec!["step one", "step two", "step three", "step four"],
    )
    .await;
    state.handle_user_message("do it all".into()).await;
    let emitted = drain(&events);
    assert_eq!(server.await.unwrap().len(), 4);
    assert_eq!(continuations(&emitted), 3);
    assert_eq!(client.request_count(), 3);
    assert_eq!(turn_ends(&emitted), vec![TurnEndReason::Completed]);
    let verdicts = verdicts(&emitted);
    assert_eq!(verdicts.len(), 4);
    assert!(verdicts[..3].iter().all(|verdict| verdict.continued));
    assert!(!verdicts[3].continued);
    assert_eq!(verdicts[3].fallback_reason.as_deref(), Some("cap_reached"));
    assert_eq!(verdicts[3].continuations_so_far, 3);

    // The next interaction starts with a fresh count.
    assert_eq!(state.continuation.count, 3);
    state.continuation.begin_turn("again");
    assert_eq!(state.continuation.count, 0);
}

#[tokio::test]
async fn client_errors_and_timeouts_hand_back_with_a_fallback_reason() {
    for (scripted, reason) in [
        (Scripted::Error, "client_error"),
        (Scripted::Hang, "timeout"),
    ] {
        let client = MockDecision::new(vec![scripted]);
        let (mut state, events, server) =
            session(Some(check(client.clone())), vec!["I'll continue."]).await;
        state.handle_user_message("go".into()).await;
        let emitted = drain(&events);
        assert_eq!(server.await.unwrap().len(), 1);
        assert_eq!(continuations(&emitted), 0);
        let verdicts = verdicts(&emitted);
        assert_eq!(verdicts.len(), 1);
        assert!(!verdicts[0].continued);
        assert_eq!(verdicts[0].fallback_reason.as_deref(), Some(reason));
        if reason == "client_error" {
            assert!(verdicts[0]
                .fallback_error
                .as_deref()
                .is_some_and(|error| error.contains("decision endpoint said no")));
        }
        assert_eq!(turn_ends(&emitted), vec![TurnEndReason::Completed]);
    }
}

#[test]
fn the_check_is_installed_only_with_a_decision_model_outside_exploration_roles() {
    let decision = DecisionConfig {
        base_url: Some("https://decision.invalid/v1".into()),
        api_key_env: "DECISION_KEY".into(),
        model: "clef:cloud".into(),
    };
    let with_decision = RigAgentConfig {
        decision: Some(decision),
        ..Default::default()
    };
    let installed = ContinuationCheck::for_session(&with_decision, None).expect("installed");
    assert_eq!(installed.model, "clef:cloud");
    assert_eq!(installed.timeout, DECISION_TIMEOUT);
    assert!(
        ContinuationCheck::for_session(&with_decision, Some(&crate::roles::EXPLORE_ROLE)).is_none()
    );
    assert!(
        ContinuationCheck::for_session(&with_decision, Some(&crate::roles::MOA_PROPOSER_ROLE))
            .is_none()
    );
    assert!(ContinuationCheck::for_session(&RigAgentConfig::default(), None).is_none());
}

#[tokio::test]
async fn no_request_without_a_check_or_with_an_empty_reply() {
    let (mut state, events, server) = session(None, vec!["I will keep going."]).await;
    state.handle_user_message("go".into()).await;
    let emitted = drain(&events);
    assert_eq!(server.await.unwrap().len(), 1);
    assert!(verdicts(&emitted).is_empty());
    assert_eq!(turn_ends(&emitted), vec![TurnEndReason::Completed]);

    let client = MockDecision::always(MODEL, 0.99);
    let (mut state, _events, _server) = session(Some(check(client.clone())), Vec::new()).await;
    let outcome = state
        .handle_continuation_check(TurnCompletion {
            stop: CompletionStop::Finished { text: "  ".into() },
            ..Default::default()
        })
        .await
        .expect("no shutdown");
    assert!(outcome.is_completing());
    assert_eq!(client.request_count(), 0);

    // The deterministic fallback (no provider model) never asks either.
    state.config.api_key_present = false;
    state
        .handle_continuation_check(TurnCompletion {
            stop: CompletionStop::Finished {
                text: "I will keep going.".into(),
            },
            ..Default::default()
        })
        .await
        .expect("no shutdown");
    assert_eq!(client.request_count(), 0);
}

#[tokio::test]
async fn a_pending_stop_or_queued_addition_is_left_to_the_turn_loop() {
    let completing = || TurnCompletion {
        stop: CompletionStop::Finished {
            text: "I will keep going.".into(),
        },
        ..Default::default()
    };

    let client = MockDecision::always(MODEL, 0.99);
    let (mut state, events, _server) = session(Some(check(client.clone())), Vec::new()).await;
    state.inbox.push_back(Command::Cancel { request_id: None });
    let outcome = state
        .handle_continuation_check(completing())
        .await
        .expect("a stop is not a shutdown");
    assert!(outcome.is_completing());
    assert!(
        state.has_pending_stop(),
        "the stop stays queued for run_turn"
    );
    assert_eq!(client.request_count(), 0);
    assert!(drain(&events).is_empty());

    let (mut state, events, _server) = session(Some(check(client.clone())), Vec::new()).await;
    state
        .inputs
        .note_environment_ready(std::path::Path::new("/tmp/wt"), "branch", "base");
    let outcome = state
        .handle_continuation_check(completing())
        .await
        .expect("no shutdown");
    assert!(outcome.is_completing());
    assert!(state.inputs.has_additions(), "the addition stays queued");
    assert_eq!(client.request_count(), 0);
    assert!(drain(&events).is_empty());

    let (mut state, _events, _server) = session(Some(check(client.clone())), Vec::new()).await;
    state.inbox.push_back(Command::Shutdown);
    assert!(state
        .handle_continuation_check(completing())
        .await
        .is_none());
    assert_eq!(client.request_count(), 0);
}

#[test]
fn the_request_carries_the_state_and_one_choice_question() {
    let request = continuation_request("clef:cloud", "please fix", "I will run it.", 4, 1);
    assert_eq!(request.model, "clef:cloud");
    let state = request.state.as_object().unwrap();
    let mut keys: Vec<_> = state.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "assistant_reply",
            "harness_continuations_so_far",
            "tool_calls_in_turn",
            "user_request"
        ]
    );
    assert_eq!(state["user_request"], "please fix");
    assert_eq!(state["assistant_reply"], "I will run it.");
    assert_eq!(state["tool_calls_in_turn"], 4);
    assert_eq!(state["harness_continuations_so_far"], 1);
    assert_eq!(request.questions.len(), 1);
    let Question::Choice {
        instructions,
        criteria,
    } = &request.questions[QUESTION]
    else {
        panic!("next_speaker must be a choice question");
    };
    assert!(instructions.ends_with("When in doubt, answer user."));
    assert_eq!(
        criteria.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![MODEL, USER]
    );
    assert!(criteria.values().all(Option::is_some));
    let body = serde_json::to_value(&request).unwrap();
    assert_eq!(body["questions"][QUESTION]["type"], "choice");

    // Bounds: the start of the request, the end of the reply, on char
    // boundaries.
    let long_request = format!("{}{}", "あ".repeat(STATE_TEXT_LIMIT), "TAIL");
    let long_reply = format!("{}{}", "HEAD", "い".repeat(STATE_TEXT_LIMIT));
    let request = continuation_request("m", &long_request, &long_reply, 0, 0);
    let user_request = request.state["user_request"].as_str().unwrap();
    let assistant_reply = request.state["assistant_reply"].as_str().unwrap();
    assert_eq!(user_request.chars().count(), STATE_TEXT_LIMIT);
    assert!(!user_request.contains("TAIL"));
    assert_eq!(assistant_reply.chars().count(), STATE_TEXT_LIMIT);
    assert!(!assistant_reply.contains("HEAD"));
}

#[test]
fn the_threshold_reads_from_its_variable_with_clamping_and_a_default() {
    assert_eq!(resolve_continuation_threshold(None), 0.8);
    assert_eq!(resolve_continuation_threshold(Some("0.5".into())), 0.5);
    assert_eq!(resolve_continuation_threshold(Some("3".into())), 1.0);
    assert_eq!(resolve_continuation_threshold(Some("-1".into())), 0.0);
    assert_eq!(resolve_continuation_threshold(Some("abc".into())), 0.8);
}
