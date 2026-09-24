//! Exercise both adapters through real HTTP/SSE, without an external provider.
use super::*;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(listener: TcpListener, rejected: bool, response: String) -> (String, Value) {
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
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(response.as_bytes()).await.unwrap();
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
            let mut history = Vec::new();
            let outcome = tokio::time::timeout(
                Duration::from_secs(5),
                complete_rig_turn(
                    &config,
                    &SessionEnvironment::for_workspace_root(None),
                    &[],
                    &mut history,
                    Message::user("read the file"),
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
                assert!(history.is_empty());
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
