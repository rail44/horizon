use super::*;
use crate::config::RigAgentConfig;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_summary_request(listener: TcpListener) -> Value {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let (body_start, body_len) = loop {
        let mut chunk = [0; 4096];
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request ended before its headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(request.len() < 1024 * 1024, "unexpectedly large request");
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .expect("JSON request content length");
            assert!(length < 1024 * 1024);
            break (end + 4, length);
        }
    };
    let received = request.len();
    request.resize(body_start + body_len, 0);
    // The first read may already contain part or all of the body.
    // Re-read only the bytes not received with the headers.
    if received < request.len() {
        socket.read_exact(&mut request[received..]).await.unwrap();
    }
    let body: Value = serde_json::from_slice(&request[body_start..]).unwrap();
    let chunk = json!({
        "id": "summary-response", "object": "chat.completion.chunk", "created": 0,
        "model": "summary-model",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Partial summary."},
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
    body
}

#[tokio::test]
async fn cap_summary_request_disables_tools_without_changing_session_config() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let request = tokio::spawn(serve_summary_request(listener));
    let key_var = "HORIZON_TEST_CAP_SUMMARY_KEY";
    std::env::set_var(key_var, "local-test-key");
    let (events_tx, events) = crossbeam_channel::unbounded();
    let mut state = SessionLoopState {
        config: RigAgentConfig {
            api_key_present: true,
            api_key_env: key_var.into(),
            base_url: Some(base_url),
            model: "summary-model".into(),
            allowed_tool_ids: Some(vec!["fs.read".into()]),
            ..Default::default()
        },
        events_tx,
        ..Default::default()
    };
    let original_config = state.config.clone();
    let result = ToolCallResult::new(
        ToolCallId("last-call".into()),
        crate::contract::OccurrenceId("last-call".into()),
        json!({"content": "read"}),
    );
    let summarized = tokio::time::timeout(
        Duration::from_secs(5),
        state.run_cap_summary_turn(&result, "fs.read"),
    )
    .await
    .expect("summary request timed out");
    assert!(summarized);
    let request = request.await.unwrap();
    assert!(
        request
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
        "the final summary must not advertise tools: {request}"
    );
    assert_eq!(state.config, original_config);
    assert!(events.try_iter().any(|event| matches!(
        event.event,
        Event::MessageCommitted(AgentMessage { role: MessageRole::Assistant, text })
            if text == "Partial summary."
    )));
}
