//! Exercise both adapters through real HTTP/SSE, without an external provider.
use super::*;
use crate::contract::ConversationInputKind;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy)]
enum Transport {
    Complete,
    Cut,
    Stall,
}

async fn serve(listener: TcpListener, rejected: bool, response: String) -> (String, Value) {
    serve_transport(&listener, rejected, response, Transport::Complete).await
}
async fn serve_transport(
    listener: &TcpListener,
    rejected: bool,
    response: String,
    transport: Transport,
) -> (String, Value) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let (start, length) = loop {
        let mut chunk = [0; 4096];
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0 && bytes.len() < 1024 * 1024);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse()
                .unwrap();
            assert!(length < 1024 * 1024);
            break (end + 4, length);
        }
    };
    let received = bytes.len();
    bytes.resize(start + length, 0);
    if received < bytes.len() {
        socket.read_exact(&mut bytes[received..]).await.unwrap();
    }
    let headers = String::from_utf8(bytes[..start].to_vec()).unwrap();
    let body = serde_json::from_slice(&bytes[start..]).unwrap();
    let status = if rejected {
        "401 Unauthorized"
    } else {
        "200 OK"
    };
    let content_type = if rejected {
        "application/json"
    } else {
        "text/event-stream"
    };
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len() + if matches!(transport, Transport::Complete) { 0 } else { 128 });
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(response.as_bytes()).await.unwrap();
    if matches!(transport, Transport::Stall) {
        std::future::pending::<()>().await;
    }
    (headers, body)
}

fn response(kind: ProviderKind) -> String {
    let frames = match kind {
        ProviderKind::OpenAiCompatible => vec![
            json!({"id":"message-1","object":"chat.completion.chunk","created":0,"model":"test-model",
                "choices":[{"index":0,"delta":{"role":"assistant","content":"reading"},"finish_reason":null}]}),
            json!({"id":"message-1","object":"chat.completion.chunk","created":0,"model":"test-model",
                "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"fs.read","arguments":"{\"path\":\"README.md\"}"}}]},"finish_reason":null}]}),
            json!({"id":"message-1","object":"chat.completion.chunk","created":0,"model":"test-model",
                "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
        ],
        ProviderKind::Anthropic => vec![
            json!({"type":"message_start","message":{"id":"message-1","role":"assistant","content":[],"model":"test-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"reading"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-1","name":"fs.read","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README.md\"}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ],
    };
    let mut sse = String::new();
    for frame in frames {
        if let Some(event) = frame["type"].as_str() {
            sse.push_str(&format!("event: {event}\n"));
        }
        sse.push_str(&format!("data: {frame}\n\n"));
    }
    if kind == ProviderKind::OpenAiCompatible {
        sse.push_str("data: [DONE]\n\n");
    }
    sse
}

#[tokio::test]
async fn provider_wire_contract_covers_both_adapters_and_rejections() {
    // nextest isolates this environment mutation; no real secret or endpoint.
    let key = "HORIZON_TEST_PROVIDER_VARIANTS_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        for rejected in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = RigAgentConfig {
                kind,
                api_key_present: true,
                api_key_env: key.into(),
                base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
                model: "test-model".into(),
                max_output_tokens: 100,
                allowed_tool_ids: Some(vec!["fs.read".into()]),
                ..Default::default()
            };
            let body = if rejected {
                json!({"error":{"type":"authentication_error","message":"test rejection"}})
                    .to_string()
            } else {
                response(kind)
            };
            let server = tokio::spawn(serve(listener, rejected, body));
            let (tx, rx) = crossbeam_channel::unbounded();
            let mut history = ConversationHistory::default();
            let outcome = tokio::time::timeout(
                Duration::from_secs(5),
                complete_rig_turn(
                    &config,
                    &SessionEnvironment::for_workspace_root(None),
                    &[],
                    &mut history,
                    Prompt::input(ConversationInputKind::User, "read the file"),
                    &tx,
                    &mut ClearingState::new(None, 80),
                    None,
                    None,
                    || panic!("must use the configured provider"),
                    &CancellationToken::new(),
                ),
            )
            .await
            .expect("local provider timed out");
            let (headers, request) = server.await.unwrap();
            assert_eq!(request["model"], "test-model");
            assert_eq!(request["max_tokens"], 100);
            assert_eq!(request["stream"], true);
            assert_eq!(request["tools"].as_array().unwrap().len(), 1);
            match kind {
                ProviderKind::OpenAiCompatible => {
                    assert!(headers.starts_with("POST /v1/chat/completions "));
                    assert!(headers
                        .to_lowercase()
                        .contains("authorization: bearer local-test-key"));
                    assert_eq!(request["parallel_tool_calls"], true);
                    assert_eq!(request["tools"][0]["function"]["name"], "fs.read");
                }
                ProviderKind::Anthropic => {
                    assert!(headers.starts_with("POST /v1/messages "));
                    assert!(headers.to_lowercase().contains("x-api-key: local-test-key"));
                    assert!(request.get("parallel_tool_calls").is_none());
                    assert_eq!(request["tools"][0]["name"], "fs.read");
                }
            }
            let events: Vec<_> = rx
                .try_iter()
                .filter_map(ProviderEvent::into_event)
                .collect();
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, Event::ProviderRequestSent(_)))
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, Event::ProviderRequestFinished))
                    .count(),
                1
            );
            assert_eq!(
                matches!(
                    outcome.stop,
                    crate::providers::rig::completion::CompletionStop::Failed
                ),
                rejected
            );
            if rejected {
                assert_eq!(history.messages(), vec![Message::user("read the file")]);
                assert!(outcome.requested_tool_call_ids.is_empty());
                assert!(events.iter().any(|e| matches!(e, Event::Error(error) if error.message.starts_with("Rig completion failed:") && error.message.contains("test rejection"))));
                assert!(!events.iter().any(|e| matches!(
                    e,
                    Event::ProviderRequestFirstToken
                        | Event::ToolCallRequested(_)
                        | Event::MessageCommitted(_)
                )));
            } else {
                assert_eq!(outcome.final_text(), Some("reading"));
                assert_eq!(
                    outcome.requested_tool_call_ids,
                    vec![ToolCallId("call-1".into())]
                );
                assert_eq!(history.len(), 2);
                assert!(events.iter().any(|e| matches!(e, Event::ToolCallRequested(call) if call.input.0 == json!({"path":"README.md"}))));
                assert!(events.iter().any(|e| matches!(e, Event::ProviderRequestUsage(usage) if usage.input_tokens == 10 && usage.output_tokens == 2 && usage.total_tokens == 12)));
            }
        }
    }
}

fn partial_response(kind: ProviderKind) -> String {
    let complete = response(kind);
    match kind {
        ProviderKind::OpenAiCompatible => complete.replace("data: [DONE]\n\n", ""),
        ProviderKind::Anthropic => complete
            .split("event: message_delta")
            .next()
            .unwrap()
            .to_owned(),
    }
}

#[tokio::test]
async fn provider_wire_interruption_retains_tools_for_both_adapters() {
    let key = "HORIZON_TEST_PROVIDER_INTERRUPTION_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        for (transport, cancel) in [
            (Transport::Cut, false),
            (Transport::Stall, false),
            (Transport::Stall, true),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = RigAgentConfig {
                kind,
                api_key_present: true,
                api_key_env: key.into(),
                base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
                model: "test-model".into(),
                max_output_tokens: 100,
                ..Default::default()
            };
            let server = tokio::spawn(async move {
                serve_transport(&listener, false, partial_response(kind), transport).await
            });
            let (tx, rx) = crossbeam_channel::unbounded();
            let token = CancellationToken::new();
            let cancel_token = token.clone();
            let cancellation = tokio::spawn(async move {
                if cancel {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    cancel_token.cancel();
                }
            });
            let deadline = ProviderDeadlines {
                establish: Duration::from_secs(3),
                idle: Duration::from_millis(250),
            };
            let environment = SessionEnvironment::for_workspace_root(None);
            let attempt = match completion_client(&config).unwrap() {
                RigCompletionClient::OpenAi(client) => {
                    run_provider_stream(
                        &config,
                        client,
                        &environment,
                        &[],
                        Message::user("read"),
                        vec![],
                        tx,
                        &token,
                        deadline,
                    )
                    .await
                }
                RigCompletionClient::Anthropic(client) => {
                    run_provider_stream(
                        &config,
                        client,
                        &environment,
                        &[],
                        Message::user("read"),
                        vec![],
                        tx,
                        &token,
                        deadline,
                    )
                    .await
                }
            };
            server.abort();
            cancellation.await.unwrap();
            let (_, completion) = match attempt {
                Attempt::Complete(value) if cancel => value,
                Attempt::Failed {
                    partial: Some(value),
                    durable_output_emitted: true,
                    ..
                } if !cancel => value,
                other => panic!("unexpected interruption for {kind:?}: {other:?}"),
            };
            assert_eq!(
                completion.stop,
                if cancel {
                    CompletionStop::Cancelled
                } else {
                    CompletionStop::Failed
                }
            );
            assert_eq!(
                completion.requested_tool_call_ids,
                [ToolCallId("call-1".into())],
                "{kind:?}"
            );
            let events: Vec<_> = rx.try_iter().collect();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event.as_event(), Some(Event::ProviderRequestSent(_))))
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event.as_event(),
                        Some(Event::ProviderRequestFinished)
                    ))
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn provider_wire_finish_reasons_override_counts_for_both_adapters() {
    let key = "HORIZON_TEST_PROVIDER_FINISH_REASON_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        for limited in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = RigAgentConfig {
                kind,
                api_key_present: true,
                api_key_env: key.into(),
                base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
                model: "test-model".into(),
                max_output_tokens: if limited { 100 } else { 2 },
                ..Default::default()
            };
            let mut body = response(kind);
            if limited {
                body = match kind {
                    ProviderKind::OpenAiCompatible => body.replace(
                        "\"finish_reason\":\"tool_calls\"",
                        "\"finish_reason\":\"length\"",
                    ),
                    ProviderKind::Anthropic => body.replace(
                        "\"stop_reason\":\"tool_use\"",
                        "\"stop_reason\":\"max_tokens\"",
                    ),
                };
            }
            let server = tokio::spawn(serve(listener, false, body));
            let (events, _) = crossbeam_channel::unbounded();
            let mut history = ConversationHistory::default();
            let completion = complete_rig_turn(
                &config,
                &SessionEnvironment::for_workspace_root(None),
                &[],
                &mut history,
                Prompt::input(ConversationInputKind::User, "read"),
                &events,
                &mut ClearingState::new(None, 80),
                None,
                None,
                || panic!("network required"),
                &CancellationToken::new(),
            )
            .await;
            server.await.unwrap();
            assert_eq!(completion.stop.truncation().is_some(), limited, "{kind:?}");
        }
    }
}

#[tokio::test]
async fn provider_wire_retry_discards_previous_text_and_never_repeats_issued_tools() {
    let key = "HORIZON_TEST_PROVIDER_RETRY_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        for issued in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = RigAgentConfig {
                kind,
                api_key_present: true,
                api_key_env: key.into(),
                base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
                model: "test-model".into(),
                max_output_tokens: 100,
                ..Default::default()
            };
            let prefix = if issued {
                partial_response(kind)
            } else {
                match kind {
                    ProviderKind::OpenAiCompatible => {
                        format!("{}\n\n", response(kind).split("\n\n").next().unwrap())
                    }
                    ProviderKind::Anthropic => response(kind)
                        .split("event: content_block_stop")
                        .next()
                        .unwrap()
                        .to_owned(),
                }
                .replace("reading", "discarded attempt")
            };
            let server = tokio::spawn(async move {
                serve_transport(&listener, false, prefix, Transport::Cut).await;
                if !issued {
                    serve_transport(&listener, false, response(kind), Transport::Complete).await;
                }
            });
            let (events, receive) = crossbeam_channel::unbounded();
            let mut history = ConversationHistory::default();
            let completion = tokio::time::timeout(
                Duration::from_secs(10),
                complete_rig_turn(
                    &config,
                    &SessionEnvironment::for_workspace_root(None),
                    &[],
                    &mut history,
                    Prompt::input(ConversationInputKind::User, "read"),
                    &events,
                    &mut ClearingState::new(None, 80),
                    None,
                    None,
                    || panic!("network required"),
                    &CancellationToken::new(),
                ),
            )
            .await
            .unwrap();
            server.await.unwrap();
            assert_eq!(
                completion.stop == CompletionStop::Failed,
                issued,
                "{kind:?}"
            );
            assert_eq!(
                completion.requested_tool_call_ids,
                [ToolCallId("call-1".into())]
            );
            let events: Vec<_> = receive
                .try_iter()
                .filter_map(ProviderEvent::into_event)
                .collect();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Event::ProviderRequestSent(_)))
                    .count(),
                if issued { 1 } else { 2 }
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Event::ToolCallRequested(_)))
                    .count(),
                1
            );
            let frame = crate::frame::agent_frame_from_events(&events);
            assert!(!frame.items.iter().any(|item| matches!(item, crate::frame::AgentFrameItem::AssistantTextDelta(delta) if delta.text.contains("discarded attempt"))));
            assert!(!serde_json::to_string(&history.messages())
                .unwrap()
                .contains("discarded attempt"));
        }
    }
}

#[tokio::test]
async fn provider_wire_multiple_calls_keep_distinct_previews_and_requests() {
    let key = "HORIZON_TEST_PROVIDER_MULTIPLE_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        let original: Vec<Value> = response(kind)
            .split("\n\n")
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .and_then(|json| serde_json::from_str(json).ok())
            })
            .collect();
        let mut frames = Vec::new();
        for frame in original {
            if kind == ProviderKind::OpenAiCompatible
                && frame["choices"][0]["delta"]["tool_calls"].is_array()
            {
                // One OpenAI response interleaves argument fragments for two calls.
                for (index, arguments) in [
                    (0, "{"),
                    (1, "{"),
                    (0, "\"path\":\"README.md\"}"),
                    (1, "\"path\":\"README.md\"}"),
                ] {
                    let mut chunk = frame.clone();
                    chunk["choices"][0]["delta"]["tool_calls"][0] = if arguments == "{" {
                        json!({"index":index,"id":format!("call-{}",index+1),"type":"function","function":{"name":"fs.read","arguments":arguments}})
                    } else {
                        json!({"index":index,"function":{"arguments":arguments}})
                    };
                    frames.push(chunk);
                }
            } else if kind == ProviderKind::Anthropic && frame["type"] == "message_delta" {
                // Anthropic delivers complete content blocks in order; both
                // calls still belong to the same parallel execution batch.
                frames.extend([
                    json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-2","name":"fs.read","input":{}}}),
                    json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README.md\"}"}}),
                    json!({"type":"content_block_stop","index":2}),
                ]);
                frames.push(frame);
            } else {
                frames.push(frame);
            }
        }
        let mut body = String::new();
        for frame in frames {
            if let Some(kind) = frame["type"].as_str() {
                body.push_str(&format!("event: {kind}\n"));
            }
            body.push_str(&format!("data: {frame}\n\n"));
        }
        if kind == ProviderKind::OpenAiCompatible {
            body.push_str("data: [DONE]\n\n");
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RigAgentConfig {
            kind,
            api_key_present: true,
            api_key_env: key.into(),
            base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
            model: "test-model".into(),
            max_output_tokens: 100,
            ..Default::default()
        };
        let server = tokio::spawn(serve(listener, false, body));
        let (events, receive) = crossbeam_channel::unbounded();
        let completion = complete_rig_turn(
            &config,
            &SessionEnvironment::for_workspace_root(None),
            &[],
            &mut ConversationHistory::default(),
            Prompt::input(ConversationInputKind::User, "read both"),
            &events,
            &mut ClearingState::new(None, 80),
            None,
            None,
            || panic!("network required"),
            &CancellationToken::new(),
        )
        .await;
        server.await.unwrap();
        let mut calls = completion.requested_tool_call_ids;
        calls.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            calls,
            [ToolCallId("call-1".into()), ToolCallId("call-2".into())]
        );
        let events: Vec<_> = receive.try_iter().collect();
        let closed: std::collections::HashSet<_> = events
            .iter()
            .filter_map(|event| {
                if let ProviderEvent::ToolCallProgressClosed(key) = event {
                    Some(key)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(closed.len(), 2);
        let live = crate::live::LiveState::with_disabled_persistence();
        live.extend_provider_events(events).unwrap();
        assert!(!live
            .frame()
            .items
            .iter()
            .any(|item| matches!(item, crate::frame::AgentFrameItem::ToolCallPreparing(_))));
    }
}

#[tokio::test]
async fn provider_wire_replay_sends_identical_requests_after_signed_tool_response() {
    let key = "HORIZON_TEST_HISTORY_REPLAY_KEY";
    std::env::set_var(key, "local-test-key");
    for kind in [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RigAgentConfig {
            kind,
            api_key_present: true,
            api_key_env: key.into(),
            base_url: Some(format!("http://{}/v1", listener.local_addr().unwrap())),
            model: "test-model".into(),
            allowed_tool_ids: Some(vec!["fs.read".into()]),
            ..Default::default()
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut live = ConversationHistory::default();
        live.open_turn(&tx);
        let mut sse = response(kind);
        if kind == ProviderKind::Anthropic {
            // A signed thinking block belongs to the same assistant response as its calls.
            let reasoning = [
                json!({"type":"content_block_start","index":2,"content_block":{"type":"thinking","thinking":""}}),
                json!({"type":"content_block_delta","index":2,"delta":{"type":"thinking_delta","thinking":"inspect first"}}),
                json!({"type":"content_block_delta","index":2,"delta":{"type":"signature_delta","signature":"opaque-signature"}}),
                json!({"type":"content_block_stop","index":2}),
            ].into_iter().map(|value|format!("event: {}\ndata: {value}\n\n",value["type"].as_str().unwrap())).collect::<String>();
            sse = sse.replacen(
                "event: message_delta",
                &(reasoning + "event: message_delta"),
                1,
            );
        }
        let server = tokio::spawn(async move {
            let _ = serve_transport(&listener, false, sse, Transport::Complete).await;
            let first = serve_transport(&listener, false, response(kind), Transport::Complete)
                .await
                .1;
            let second = serve_transport(&listener, false, response(kind), Transport::Complete)
                .await
                .1;
            (first, second)
        });
        let outcome = complete_rig_turn(
            &config,
            &SessionEnvironment::for_workspace_root(None),
            &[],
            &mut live,
            Prompt::input(ConversationInputKind::User, "read"),
            &tx,
            &mut ClearingState::disabled(),
            None,
            None,
            || panic!("HTTP expected"),
            &CancellationToken::new(),
        )
        .await;
        let call = outcome
            .requested_tool_calls
            .values()
            .next()
            .expect("tool response");
        let result = call.identity.result(json!({"content":"file data"}));
        live.append_result(&result, &call.tool_id).unwrap();
        tx.send(Event::ToolCallFinished(result).into()).unwrap();
        let events: Vec<_> = rx
            .try_iter()
            .filter_map(ProviderEvent::into_event)
            .collect();
        let bytes = serde_json::to_vec(&events).unwrap();
        let mut replay = ConversationHistory::from_events(
            &serde_json::from_slice::<Vec<Event>>(&bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(live.messages(), replay.messages());
        for history in [&mut live, &mut replay] {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                complete_rig_turn(
                    &config,
                    &SessionEnvironment::for_workspace_root(None),
                    &[],
                    history,
                    Prompt::input(ConversationInputKind::Notification, "task finished"),
                    &tx,
                    &mut ClearingState::disabled(),
                    None,
                    None,
                    || panic!("HTTP expected"),
                    &CancellationToken::new(),
                ),
            )
            .await
            .unwrap();
            assert!(!matches!(result.stop, CompletionStop::Failed));
        }
        let (first, second) = server.await.unwrap();
        assert_eq!(first, second, "{kind:?} request differs after replay");
        if kind == ProviderKind::Anthropic {
            assert!(
                first.to_string().contains("opaque-signature"),
                "signed reasoning must reach the next request: {first}"
            );
        }
    }
}
